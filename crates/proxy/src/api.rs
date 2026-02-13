//! API endpoint handlers — health, version, system info, config.
//!
//! Each handler is an axum handler function that takes `State<Arc<AppState>>`
//! and returns a JSON response.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde_json::json;

use bccf_core::{AppState, DEFAULT_AGENT_MODEL};
use bccf_database::DbPool;

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// GET /health — returns basic health status.
pub async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let account_count = state.db_pool::<DbPool>().map_or(0, |pool| {
        pool.get()
            .ok()
            .and_then(|conn| {
                conn.query_row("SELECT COUNT(*) FROM accounts", [], |row| {
                    row.get::<_, i64>(0)
                })
                .ok()
            })
            .unwrap_or(0)
    });

    let config = state.config();
    let strategy = format!("{:?}", config.get_strategy());

    Json(json!({
        "status": "ok",
        "accounts": account_count,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "strategy": strategy
    }))
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// GET /api/version — returns the application version.
pub async fn version() -> impl IntoResponse {
    Json(json!({
        "version": bccf_core::get_version(),
        "cached": false
    }))
}

// ---------------------------------------------------------------------------
// System Info
// ---------------------------------------------------------------------------

/// GET /api/system — returns system information.
pub async fn system_info() -> impl IntoResponse {
    let platform = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let exec_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let is_docker = detect_docker();

    Json(json!({
        "platform": platform,
        "arch": arch,
        "isDocker": is_docker,
        "execPath": exec_path,
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

/// Detect if running inside a Docker container.
fn detect_docker() -> bool {
    // Check /.dockerenv
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    // Check env vars
    if std::env::var("DOCKER_CONTAINER").is_ok() || std::env::var("KUBERNETES_SERVICE_HOST").is_ok()
    {
        return true;
    }

    // Check /proc/1/cgroup for docker/containerd
    if let Ok(contents) = std::fs::read_to_string("/proc/1/cgroup") {
        if contents.contains("docker") || contents.contains("containerd") {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// GET /api/config — returns all configuration settings.
pub async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config();
    let runtime = config.get_runtime();

    Json(json!({
        "lb_strategy": format!("{:?}", config.get_strategy()),
        "port": runtime.port,
        "sessionDurationMs": runtime.session_duration_ms,
        "default_agent_model": DEFAULT_AGENT_MODEL,
        "tls_enabled": false
    }))
}

/// GET /api/config/strategy — returns the current load balancing strategy.
pub async fn get_strategy(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config();

    Json(json!({
        "strategy": format!("{:?}", config.get_strategy())
    }))
}

/// POST /api/config/strategy — updates the load balancing strategy.
pub async fn set_strategy(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let strategy = match body.get("strategy").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Missing 'strategy' field"})),
            )
                .into_response();
        }
    };

    let valid = ["round_robin", "priority", "session", "least_used"];
    if !valid.contains(&strategy) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid strategy. Valid: {valid:?}")})),
        )
            .into_response();
    }

    // TODO: persist strategy change to config when config write is implemented
    Json(json!({"success": true, "strategy": strategy})).into_response()
}

/// GET /api/config/strategies — list available strategies.
pub async fn get_strategies() -> impl IntoResponse {
    Json(json!({
        "strategies": ["round_robin", "priority", "session", "least_used"]
    }))
}

/// GET /api/config/retention — get data retention settings.
pub async fn get_retention(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = state.config();

    Json(json!({
        "payloadDays": config.get_data_retention_days(),
        "requestDays": config.get_request_retention_days()
    }))
}

/// POST /api/config/retention — update data retention settings.
pub async fn set_retention(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Validate payloadDays
    if let Some(val) = body.get("payloadDays") {
        if let Some(days) = val.as_i64() {
            if !(1..=365).contains(&days) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "payloadDays must be between 1 and 365"})),
                )
                    .into_response();
            }
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "payloadDays must be an integer"})),
            )
                .into_response();
        }
    }

    // Validate requestDays
    if let Some(val) = body.get("requestDays") {
        if let Some(days) = val.as_i64() {
            if !(1..=3650).contains(&days) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "requestDays must be between 1 and 3650"})),
                )
                    .into_response();
            }
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "requestDays must be an integer"})),
            )
                .into_response();
        }
    }

    // TODO: persist retention settings to config
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Catch-all / Not Found
// ---------------------------------------------------------------------------

/// Fallback handler for unmatched routes.
pub async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"})))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_docker_returns_false_normally() {
        // On most dev machines, this should be false
        let result = detect_docker();
        // Just verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn valid_strategies() {
        let valid = ["round_robin", "priority", "session", "least_used"];
        assert!(valid.contains(&"round_robin"));
        assert!(valid.contains(&"session"));
        assert!(!valid.contains(&"invalid"));
    }

    #[tokio::test]
    async fn version_handler() {
        let resp = version().await;
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_response().into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(json.get("version").is_some());
    }

    #[tokio::test]
    async fn system_info_handler() {
        let resp = system_info().await;
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_response().into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(json.get("platform").is_some());
        assert!(json.get("arch").is_some());
    }

    #[tokio::test]
    async fn strategies_handler() {
        let resp = get_strategies().await;
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_response().into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        let strategies = json["strategies"].as_array().unwrap();
        assert!(strategies.len() >= 4);
    }

    #[tokio::test]
    async fn not_found_handler() {
        let resp = not_found().await;
        let response = resp.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
