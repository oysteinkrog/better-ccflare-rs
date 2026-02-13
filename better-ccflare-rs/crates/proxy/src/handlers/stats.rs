//! Stats API handlers.
//!
//! - `GET /api/stats` — aggregate statistics

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde_json::json;
use tracing::error;

use bccf_core::AppState;
use bccf_database::repositories::stats as stats_repo;
use bccf_database::DbPool;

// ---------------------------------------------------------------------------
// GET /api/stats
// ---------------------------------------------------------------------------

/// Aggregate statistics: total requests, tokens, cost, by account, by model.
pub async fn get_stats(State(state): State<Arc<AppState>>) -> Response {
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

    let aggregated = match stats_repo::get_aggregated_stats(&conn) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to get aggregated stats: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    let active_accounts = stats_repo::get_active_account_count(&conn).unwrap_or(0);
    let account_stats = stats_repo::get_account_stats(&conn, 20).unwrap_or_default();
    let top_models = stats_repo::get_top_models(&conn, 10).unwrap_or_default();
    let recent_errors = stats_repo::get_recent_errors(&conn, 20).unwrap_or_default();

    let success_rate = if aggregated.total_requests > 0 {
        (aggregated.successful_requests as f64 / aggregated.total_requests as f64) * 100.0
    } else {
        0.0
    };

    let accounts_json: Vec<serde_json::Value> = account_stats
        .iter()
        .map(|a| {
            json!({
                "name": a.name,
                "requests": a.request_count,
                "successRate": a.success_rate,
            })
        })
        .collect();

    let models_json: Vec<serde_json::Value> = top_models
        .iter()
        .map(|m| {
            json!({
                "model": m.model,
                "count": m.count,
            })
        })
        .collect();

    Json(json!({
        "totalRequests": aggregated.total_requests,
        "successRate": success_rate,
        "activeAccounts": active_accounts,
        "avgResponseTime": aggregated.avg_response_time,
        "totalTokens": aggregated.total_tokens,
        "totalCostUsd": aggregated.total_cost_usd,
        "topModels": models_json,
        "avgTokensPerSecond": aggregated.avg_tokens_per_second,
        "accounts": accounts_json,
        "recentErrors": recent_errors,
    }))
    .into_response()
}
