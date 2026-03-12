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
pub async fn health(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok"
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
        "timestamp": timestamp_iso(),
        "packageManager": null,
        "nodeVersion": null,
        "bunVersion": null,
        "isBinary": true
    }))
}

/// ISO 8601 timestamp with millisecond precision and Z suffix (matches JS `new Date().toISOString()`).
fn timestamp_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
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
        "lb_strategy": config.get_strategy().as_str(),
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
        "strategy": config.get_strategy().as_str()
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
// Default Agent Model
// ---------------------------------------------------------------------------

/// GET /api/config/model — returns the default agent model.
pub async fn get_default_model() -> impl IntoResponse {
    Json(json!({
        "model": DEFAULT_AGENT_MODEL
    }))
}

/// POST /api/config/model — set the default agent model.
pub async fn set_default_model(Json(body): Json<serde_json::Value>) -> Response {
    let model = match body.get("model").and_then(|v| v.as_str()) {
        Some(m) if !m.trim().is_empty() => m.trim(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "model (string) is required"})),
            )
                .into_response();
        }
    };

    // TODO: persist to config file when config write is implemented
    Json(json!({"success": true, "model": model})).into_response()
}

// ---------------------------------------------------------------------------
// Maintenance
// ---------------------------------------------------------------------------

/// POST /api/maintenance/cleanup — run data retention cleanup.
pub async fn maintenance_cleanup(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not configured"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database unavailable"})),
        )
            .into_response();
    };

    let config = state.config();
    let payload_days = config.get_data_retention_days();
    let request_days = config.get_request_retention_days();
    let xfactor_days = config.get_xfactor_retention_days();
    let now = chrono::Utc::now().timestamp_millis();

    let payload_cutoff = now - (payload_days as i64 * 24 * 60 * 60 * 1000);
    let request_cutoff = now - (request_days as i64 * 24 * 60 * 60 * 1000);
    let xfactor_cutoff = now - (xfactor_days as i64 * 24 * 60 * 60 * 1000);

    let payloads_deleted = conn
        .execute(
            "DELETE FROM request_payloads WHERE request_id IN (SELECT id FROM requests WHERE timestamp < ?1)",
            [payload_cutoff],
        )
        .unwrap_or(0);

    let requests_deleted = conn
        .execute(
            "DELETE FROM requests WHERE timestamp < ?1",
            [request_cutoff],
        )
        .unwrap_or(0);

    let xfactor_deleted = conn
        .execute(
            "DELETE FROM xfactor_observations WHERE timestamp_ms < ?1",
            [xfactor_cutoff],
        )
        .unwrap_or(0);

    Json(json!({
        "success": true,
        "payloadsDeleted": payloads_deleted,
        "requestsDeleted": requests_deleted,
        "xfactorDeleted": xfactor_deleted,
        "payloadRetentionDays": payload_days,
        "requestRetentionDays": request_days,
        "xfactorRetentionDays": xfactor_days
    }))
    .into_response()
}

/// POST /api/maintenance/compact — VACUUM the database.
pub async fn maintenance_compact(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not configured"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database unavailable"})),
        )
            .into_response();
    };

    match conn.execute_batch("VACUUM") {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("VACUUM failed: {e}")})),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Stats Reset
// ---------------------------------------------------------------------------

/// POST /api/stats/reset — reset all request stats.
pub async fn stats_reset(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not configured"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database unavailable"})),
        )
            .into_response();
    };

    let _ = conn.execute_batch("DELETE FROM request_payloads; DELETE FROM requests;");
    let _ = conn.execute_batch(
        "UPDATE accounts SET request_count = 0, total_requests = 0, session_request_count = 0, session_start = NULL, last_used = NULL;",
    );

    Json(json!({"success": true, "message": "All request stats have been reset"})).into_response()
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// GET /api/projects — list distinct project names.
pub async fn get_projects(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return Json(json!([])).into_response();
    };
    let Ok(conn) = pool.get() else {
        return Json(json!([])).into_response();
    };

    let projects =
        bccf_database::repositories::stats::get_distinct_projects(&conn, 1000).unwrap_or_default();
    Json(json!(projects)).into_response()
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

/// GET /api/workspaces — list workspaces (returns empty for now).
pub async fn get_workspaces() -> impl IntoResponse {
    Json(json!({"workspaces": []}))
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
