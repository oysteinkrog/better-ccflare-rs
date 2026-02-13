//! HTTP server — axum-based server with routing, middleware, and startup tasks.
//!
//! Binds to the configured host:port, mounts all routes (proxy, API, health),
//! applies auth and CORS middleware, runs startup maintenance, and handles
//! graceful shutdown on SIGTERM/SIGINT.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use bccf_core::constants::network;
use bccf_core::AppState;
use bccf_database::DbPool;

use crate::accounts;
use crate::api;
use crate::auth;
use crate::handlers;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Host to bind to (default: "0.0.0.0").
    pub host: String,
    /// Port to bind to (default: 8080).
    pub port: u16,
    /// Whether to enable TLS.
    pub tls_enabled: bool,
    /// Path to TLS certificate file.
    pub tls_cert_path: Option<String>,
    /// Path to TLS key file.
    pub tls_key_path: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: network::DEFAULT_PORT,
            tls_enabled: false,
            tls_cert_path: None,
            tls_key_path: None,
        }
    }
}

impl ServerConfig {
    /// Create from environment variables and AppState config.
    pub fn from_env(state: &AppState) -> Self {
        let config = state.config();
        let runtime = config.get_runtime();

        let host = std::env::var("BETTER_CCFLARE_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(runtime.port);

        let tls_cert_path = std::env::var("SSL_CERT_PATH").ok();
        let tls_key_path = std::env::var("SSL_KEY_PATH").ok();
        let tls_enabled = tls_cert_path.is_some() && tls_key_path.is_some();

        Self {
            host,
            port,
            tls_enabled,
            tls_cert_path,
            tls_key_path,
        }
    }
}

/// Build the axum router with all routes and middleware.
pub fn build_router(state: Arc<AppState>) -> Router {
    // API routes (require auth via middleware)
    let api_routes = Router::new()
        // Health (also mounted at root level, exempt from auth)
        .route("/api/version", get(api::version))
        .route("/api/system/info", get(api::system_info))
        // Config
        .route("/api/config", get(api::get_config))
        .route(
            "/api/config/strategy",
            get(api::get_strategy).post(api::set_strategy),
        )
        .route("/api/config/strategies", get(api::get_strategies))
        .route(
            "/api/config/retention",
            get(api::get_retention).post(api::set_retention),
        )
        // Account management
        .route("/api/accounts", get(accounts::list_accounts))
        .route("/api/accounts/{id}/pause", post(accounts::pause_account))
        .route("/api/accounts/{id}/resume", post(accounts::resume_account))
        .route("/api/accounts/{id}/reload", post(accounts::reload_account))
        .route(
            "/api/accounts/{id}/priority",
            post(accounts::update_priority),
        )
        .route("/api/accounts/{id}/rename", post(accounts::rename_account))
        .route("/api/accounts/{id}", delete(accounts::delete_account))
        .route(
            "/api/accounts/{id}/auto-fallback",
            post(accounts::set_auto_fallback),
        )
        // Request history & payload
        .route("/api/requests", get(handlers::requests::list_requests))
        .route(
            "/api/requests/{id}/payload",
            get(handlers::requests::get_request_payload),
        )
        // SSE streams
        .route(
            "/api/requests/stream",
            get(handlers::streams::request_events_stream),
        )
        .route("/api/logs/stream", get(handlers::logs::logs_stream))
        // Stats & analytics
        .route("/api/stats", get(handlers::stats::get_stats))
        .route("/api/analytics", get(handlers::analytics::get_analytics))
        // Logs history
        .route("/api/logs", get(handlers::logs::logs_history))
        // Token health
        .route(
            "/api/token-health",
            get(handlers::token_health::get_token_health),
        )
        .route(
            "/api/token-health/reauth",
            get(handlers::token_health::get_reauth_needed),
        )
        .route(
            "/api/token-health/{account_name}",
            get(handlers::token_health::get_account_token_health),
        )
        // API key management
        .route(
            "/api/keys",
            get(handlers::api_keys::list_keys).post(handlers::api_keys::generate_key),
        )
        .route("/api/keys/stats", get(handlers::api_keys::key_stats))
        .route("/api/keys/{id}", delete(handlers::api_keys::delete_key))
        .route(
            "/api/keys/{id}/enable",
            post(handlers::api_keys::enable_key),
        )
        .route(
            "/api/keys/{id}/disable",
            post(handlers::api_keys::disable_key),
        )
        // Agent preferences
        .route("/api/agents", get(handlers::agents::list_agents))
        .route(
            "/api/agents/{id}/model",
            post(handlers::agents::update_agent_model),
        );

    // Dashboard routes (exempt from API auth — served under /dashboard)
    let dashboard_routes = bccf_dashboard::routes::router();

    // Combine all routes
    Router::new()
        // Health endpoint (exempt from auth)
        .route("/health", get(api::health))
        // Dashboard (exempt from auth, not under /api/)
        .merge(dashboard_routes)
        // API routes (with auth middleware)
        .merge(api_routes)
        // Fallback for unmatched routes
        .fallback(api::not_found)
        // Middleware layers (applied bottom-up)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Start the HTTP server.
///
/// Binds to the configured address, runs startup maintenance in the background,
/// and serves requests until a shutdown signal is received.
pub async fn start(state: Arc<AppState>, server_config: ServerConfig) -> Result<(), ServerError> {
    let addr: SocketAddr = format!("{}:{}", server_config.host, server_config.port)
        .parse()
        .map_err(|e| ServerError::Bind(format!("Invalid address: {e}")))?;

    let app = build_router(state.clone());

    // Run startup maintenance (fire-and-forget)
    let startup_state = state.clone();
    tokio::spawn(async move {
        run_startup_maintenance(&startup_state).await;
    });

    info!("Starting server on {addr}");

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| ServerError::Bind(format!("Failed to bind {addr}: {e}")))?;

    info!("Server listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown::shutdown_signal())
        .await
        .map_err(|e| ServerError::Serve(e.to_string()))?;

    info!("Server shut down gracefully");
    Ok(())
}

/// Run startup maintenance tasks (fire-and-forget on boot).
///
/// - Clear expired rate limits
/// - Cleanup expired OAuth sessions
/// - Data retention cleanup is handled by the retention service (US-027)
async fn run_startup_maintenance(state: &AppState) {
    info!("Running startup maintenance...");

    let Some(pool) = state.db_pool::<DbPool>() else {
        warn!("No database pool — skipping startup maintenance");
        return;
    };

    let now = chrono::Utc::now().timestamp_millis();

    // Clear expired rate limits
    match clear_expired_rate_limits(pool, now) {
        Ok(count) => {
            if count > 0 {
                info!("Cleared {count} expired rate limit entries");
            }
        }
        Err(e) => warn!("Failed to clear expired rate limits: {e}"),
    }

    // Clear expired OAuth sessions (tokens past expiry)
    match clear_expired_oauth_sessions(pool, now) {
        Ok(count) => {
            if count > 0 {
                info!("Cleared {count} expired OAuth sessions");
            }
        }
        Err(e) => warn!("Failed to clear expired OAuth sessions: {e}"),
    }

    info!("Startup maintenance complete");
}

/// Clear rate_limited_until entries that are in the past.
fn clear_expired_rate_limits(pool: &DbPool, now: i64) -> Result<usize, String> {
    let conn = pool.get().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE accounts SET rate_limited_until = NULL WHERE rate_limited_until IS NOT NULL AND rate_limited_until < ?1",
        [now],
    )
    .map_err(|e| e.to_string())
}

/// Clear expired OAuth sessions (where expires_at < now and provider is OAuth).
fn clear_expired_oauth_sessions(pool: &DbPool, now: i64) -> Result<usize, String> {
    let conn = pool.get().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE accounts SET access_token = NULL, expires_at = NULL WHERE expires_at IS NOT NULL AND expires_at < ?1 AND provider IN ('claude-oauth', 'console')",
        [now],
    )
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Server startup errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to bind: {0}")]
    Bind(String),
    #[error("Server error: {0}")]
    Serve(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use bccf_core::config::Config;
    use http::Request;
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-server-nonexistent/config.json",
        )))
        .unwrap();
        Arc::new(AppState::new(config))
    }

    #[tokio::test]
    async fn health_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn version_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/version")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("version").is_some());
    }

    #[tokio::test]
    async fn system_info_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/system/info")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("platform").is_some());
    }

    #[tokio::test]
    async fn config_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("port").is_some());
        assert!(json.get("lb_strategy").is_some());
    }

    #[tokio::test]
    async fn strategies_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/config/strategies")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn not_found_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn server_config_defaults() {
        let config = ServerConfig::default();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert!(!config.tls_enabled);
    }

    #[tokio::test]
    async fn retention_endpoint() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .uri("/api/config/retention")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("payloadDays").is_some());
    }

    #[tokio::test]
    async fn set_strategy_valid() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/config/strategy")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"strategy":"session"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn set_strategy_invalid() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/config/strategy")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"strategy":"nonexistent"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn cors_headers_present() {
        let state = test_state();
        let app = build_router(state);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/health")
            .header("origin", "http://localhost:3000")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // CORS layer should handle OPTIONS
        assert!(
            resp.headers().contains_key("access-control-allow-origin")
                || resp.status().is_success()
        );
    }
}
