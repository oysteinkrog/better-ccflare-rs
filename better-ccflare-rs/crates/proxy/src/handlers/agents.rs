//! Agent preference endpoints — model preferences per agent.
//!
//! - `GET  /api/agents` — list agents with their model preferences
//! - `POST /api/agents/:id/model` — set model preference for an agent

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use bccf_core::models::{is_valid_model_id, ALL_MODEL_IDS, DEFAULT_AGENT_MODEL};
use bccf_core::AppState;
use bccf_database::repositories::agent_preference;
use bccf_database::DbPool;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/agents — list all agent preferences.
///
/// Returns stored agent model preferences from the database.
/// In a full deployment agents are discovered from filesystem scanning;
/// here we return the DB-stored preferences which is what the dashboard needs.
pub async fn list_agents(State(state): State<Arc<AppState>>) -> Response {
    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    match agent_preference::get_all_preferences(&conn) {
        Ok(prefs) => {
            let agents: Vec<serde_json::Value> = prefs
                .iter()
                .map(|p| {
                    json!({
                        "id": p.agent_id,
                        "model": p.preferred_model,
                        "updatedAt": chrono::DateTime::from_timestamp_millis(p.updated_at)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default()
                    })
                })
                .collect();

            Json(json!({
                "agents": agents,
                "defaultModel": DEFAULT_AGENT_MODEL
            }))
            .into_response()
        }
        Err(e) => {
            warn!("Failed to list agents: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to list agents"})),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateModelRequest {
    model: Option<String>,
}

/// POST /api/agents/:id/model — set model preference for an agent.
pub async fn update_agent_model(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    Json(body): Json<UpdateModelRequest>,
) -> Response {
    let model = match body.model {
        Some(ref m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "model is required"})),
            )
                .into_response();
        }
    };

    if !is_valid_model_id(&model) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("Invalid model. Allowed: {:?}", ALL_MODEL_IDS)
            })),
        )
            .into_response();
    }

    let conn = match get_conn(&state) {
        Ok(c) => c,
        Err(r) => return r,
    };

    let now = chrono::Utc::now().timestamp_millis();

    match agent_preference::set_preference(&conn, &agent_id, &model, now) {
        Ok(()) => Json(json!({
            "success": true,
            "agentId": agent_id,
            "model": model
        }))
        .into_response(),
        Err(e) => {
            warn!("Failed to update agent {agent_id} model: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to update agent model"})),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use bccf_core::config::Config;
    use bccf_core::state::AppStateBuilder;
    use bccf_database::schema;
    use http::Request;
    use tower::ServiceExt;

    fn test_state_with_db() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-agents-nonexistent/config.json",
        )))
        .unwrap();

        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();

        {
            let conn = pool.get().unwrap();
            schema::create_tables(&conn).unwrap();
            schema::create_indexes(&conn).unwrap();
        }

        Arc::new(AppStateBuilder::new(config).db_pool(pool).build())
    }

    fn test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/agents", get(list_agents))
            .route("/api/agents/{id}/model", post(update_agent_model))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_agents_empty() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["agents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_agent_model_valid() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents/agent-1/model")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model": "claude-sonnet-4-5-20250929"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["agentId"], "agent-1");
    }

    #[tokio::test]
    async fn update_agent_model_invalid() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents/agent-1/model")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model": "gpt-4"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn update_agent_model_missing() {
        let state = test_state_with_db();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agents/agent-1/model")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }
}
