//! API key management endpoints — CRUD for proxy API keys.
//!
//! - `POST /api/keys` — generate a new key (returns plaintext once)
//! - `GET  /api/keys` — list keys (last 8 chars only)
//! - `DELETE /api/keys/:id` — permanently delete a key
//! - `POST /api/keys/:id/enable` — re-enable a disabled key
//! - `POST /api/keys/:id/disable` — disable a key

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use bccf_core::types::{ApiKey, ApiKeyResponse, KeyScope};
use bccf_core::AppState;
use bccf_database::repositories::api_key as api_key_repo;
use bccf_database::DbPool;

use crate::crypto;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(clippy::result_large_err)]
#[allow(clippy::result_large_err)]
fn get_conn(
    state: &AppState,
) -> Result<r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>, Response> {
    let pool = state.db_pool::<DbPool>().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not available"})),
        )
            .into_response()
    })?;
    pool.get().map_err(|e| {
        warn!("Failed to get DB connection: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database connection error"})),
        )
            .into_response()
    })
}

fn to_api_key_response(key: &ApiKey) -> ApiKeyResponse {
    ApiKeyResponse {
        id: key.id.clone(),
        name: key.name.clone(),
        prefix_last_8: key.prefix_last_8.clone(),
        created_at: chrono::DateTime::from_timestamp_millis(key.created_at)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        last_used: key
            .last_used
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|dt| dt.to_rfc3339()),
        usage_count: key.usage_count,
        is_active: key.is_active,
        scope: key.scope,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct GenerateKeyRequest {
    name: Option<String>,
    #[serde(default)]
    scope: KeyScope,
}

/// POST /api/keys — generate a new API key.
pub async fn generate_key(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GenerateKeyRequest>,
) -> Response {
    let name = match body.name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Name is required"})),
            )
                .into_response();
        }
    };

    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    // Generate key and hash
    let plaintext = crypto::generate_api_key();
    let hashed = match crypto::hash_api_key(&plaintext) {
        Ok(h) => h,
        Err(e) => {
            warn!("Failed to hash API key: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to generate key"})),
            )
                .into_response();
        }
    };

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let key = ApiKey {
        id: id.clone(),
        name: name.clone(),
        hashed_key: hashed,
        prefix_last_8: crypto::key_suffix(&plaintext),
        created_at: now,
        last_used: None,
        usage_count: 0,
        is_active: true,
        scope: body.scope,
    };

    if let Err(e) = api_key_repo::create(&conn, &key) {
        warn!("Failed to create API key: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to save key"})),
        )
            .into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "data": {
                "id": id,
                "name": name,
                "apiKey": plaintext,
                "prefixLast8": crypto::key_suffix(&plaintext),
                "createdAt": chrono::DateTime::from_timestamp_millis(now)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            }
        })),
    )
        .into_response()
}

/// GET /api/keys — list all API keys.
pub async fn list_keys(State(state): State<Arc<AppState>>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    match api_key_repo::find_all(&conn) {
        Ok(keys) => {
            let data: Vec<ApiKeyResponse> = keys.iter().map(to_api_key_response).collect();
            let count = data.len();
            Json(json!({"success": true, "data": data, "count": count})).into_response()
        }
        Err(e) => {
            warn!("Failed to list API keys: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to list keys"})),
            )
                .into_response()
        }
    }
}

/// DELETE /api/keys/:id — permanently delete a key.
pub async fn delete_key(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    match api_key_repo::delete(&conn, &id) {
        Ok(true) => Json(json!({"success": true, "message": format!("API key {id} deleted")}))
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("API key {id} not found")})),
        )
            .into_response(),
        Err(e) => {
            warn!("Failed to delete API key {id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to delete key"})),
            )
                .into_response()
        }
    }
}

/// POST /api/keys/:id/enable — re-enable a disabled key.
pub async fn enable_key(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    match api_key_repo::enable(&conn, &id) {
        Ok(true) => Json(json!({"success": true, "message": format!("API key {id} enabled")}))
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("API key {id} not found")})),
        )
            .into_response(),
        Err(e) => {
            warn!("Failed to enable API key {id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to enable key"})),
            )
                .into_response()
        }
    }
}

/// POST /api/keys/:id/disable — disable a key.
pub async fn disable_key(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    match api_key_repo::disable(&conn, &id) {
        Ok(true) => Json(json!({"success": true, "message": format!("API key {id} disabled")}))
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("API key {id} not found")})),
        )
            .into_response(),
        Err(e) => {
            warn!("Failed to disable API key {id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to disable key"})),
            )
                .into_response()
        }
    }
}

/// GET /api/keys/stats — key usage statistics.
pub async fn key_stats(State(state): State<Arc<AppState>>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    let total = api_key_repo::count_all(&conn).unwrap_or(0);
    let active = api_key_repo::count_active(&conn).unwrap_or(0);
    let inactive = total - active;

    Json(json!({
        "success": true,
        "data": {
            "total": total,
            "active": active,
            "inactive": inactive
        }
    }))
    .into_response()
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
    use bccf_database::schema;
    use http::Request;
    use tower::ServiceExt;

    fn test_state_with_db() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-api-keys-nonexistent/config.json",
        )))
        .unwrap();

        // Create in-memory DB with schema
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();

        // Initialize schema
        {
            let conn = pool.get().unwrap();
            schema::create_tables(&conn).unwrap();
            schema::create_indexes(&conn).unwrap();
        }

        let state = AppStateBuilder::new(config).db_pool(pool).build();
        Arc::new(state)
    }

    fn test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/keys", get(list_keys).post(generate_key))
            .route("/api/keys/stats", get(key_stats))
            .route("/api/keys/{id}", delete(delete_key))
            .route("/api/keys/{id}/enable", post(enable_key))
            .route("/api/keys/{id}/disable", post(disable_key))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_keys_empty() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/keys")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["count"], 0);
    }

    #[tokio::test]
    async fn generate_key_success() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Test Key"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 201);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);

        let api_key = json["data"]["apiKey"].as_str().unwrap();
        assert_eq!(api_key.len(), 64); // 32 bytes = 64 hex chars
        assert_eq!(json["data"]["name"], "Test Key");
    }

    #[tokio::test]
    async fn generate_key_missing_name() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn delete_nonexistent_key() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/keys/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn key_stats_works() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/keys/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["total"], 0);
        assert_eq!(json["data"]["active"], 0);
    }

    #[tokio::test]
    async fn enable_nonexistent_key() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys/nonexistent/enable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn scrypt_hash_verifies() {
        // Ensure the crypto module's hash/verify roundtrips work
        let key = crypto::generate_api_key();
        let hash = crypto::hash_api_key(&key).unwrap();
        assert!(crypto::verify_api_key(&key, &hash).unwrap());
        assert!(!crypto::verify_api_key("wrong-key", &hash).unwrap());
    }
}
