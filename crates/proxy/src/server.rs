//! HTTP server — axum-based server with routing, middleware, and startup tasks.
//!
//! Binds to the configured host:port, mounts all routes (proxy, API, health),
//! applies auth and CORS middleware, runs startup maintenance, and handles
//! graceful shutdown on SIGTERM/SIGINT.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{any, delete, get, post};
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
use crate::token_manager::{TokenManager, TokenPersister, TokenRefresher};

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
        .route("/api/system", get(api::system_info))
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
        .route(
            "/api/config/model",
            get(api::get_default_model).post(api::set_default_model),
        )
        // Account management
        .route(
            "/api/accounts",
            get(accounts::list_accounts).post(accounts::create_account),
        )
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
        .route(
            "/api/accounts/{id}/auto-refresh",
            post(accounts::set_auto_refresh),
        )
        .route(
            "/api/accounts/{id}/custom-endpoint",
            post(accounts::set_custom_endpoint),
        )
        .route(
            "/api/accounts/{id}/model-mappings",
            post(accounts::set_model_mappings),
        )
        .route(
            "/api/accounts/{id}/reserve-5h",
            post(accounts::set_reserve_5h),
        )
        .route(
            "/api/accounts/{id}/reserve-weekly",
            post(accounts::set_reserve_weekly),
        )
        .route(
            "/api/accounts/{id}/reserve-hard",
            post(accounts::set_reserve_hard),
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
        .route("/api/stats/reset", post(api::stats_reset))
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
        // Aliases for /api/api-keys (TS uses this path)
        .route(
            "/api/api-keys",
            get(handlers::api_keys::list_keys).post(handlers::api_keys::generate_key),
        )
        .route("/api/api-keys/stats", get(handlers::api_keys::key_stats))
        .route(
            "/api/api-keys/{id}",
            delete(handlers::api_keys::delete_key),
        )
        .route(
            "/api/api-keys/{id}/enable",
            post(handlers::api_keys::enable_key),
        )
        .route(
            "/api/api-keys/{id}/disable",
            post(handlers::api_keys::disable_key),
        )
        // Maintenance
        .route(
            "/api/maintenance/cleanup",
            post(api::maintenance_cleanup),
        )
        .route(
            "/api/maintenance/compact",
            post(api::maintenance_compact),
        )
        // Projects & workspaces
        .route("/api/projects", get(api::get_projects))
        .route("/api/workspaces", get(api::get_workspaces))
        // Agent preferences
        .route("/api/agents", get(handlers::agents::list_agents))
        .route(
            "/api/agents/{id}/model",
            post(handlers::agents::update_agent_model),
        )
        .route(
            "/api/agents/bulk-preference",
            post(handlers::agents::bulk_agent_preference),
        )
        // OAuth re-authentication
        .route("/api/oauth/init/{id}", post(crate::oauth::oauth_init))
        .route("/api/oauth/callback", get(crate::oauth::oauth_callback))
        .route("/api/oauth/complete", post(crate::oauth::oauth_complete))
        // Proxy routes — core /v1/messages endpoint
        .route("/v1/messages", post(crate::proxy::proxy_handler))
        .route("/v1/{*rest}", any(crate::proxy::proxy_handler));

    // Dashboard routes (exempt from API auth — served under /dashboard)
    let dashboard_routes = bccf_dashboard::routes::router();

    // Combine all routes
    Router::new()
        // Root redirect to dashboard
        .route("/", get(|| async {
            axum::response::Redirect::temporary("/dashboard")
        }))
        // Health endpoint (exempt from auth)
        .route("/health", get(api::health))
        // Prometheus metrics (exempt from auth, optional — returns 503 if not enabled)
        .route("/metrics", get(crate::prometheus::metrics_handler))
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
///
/// When a shutdown signal arrives, axum stops accepting new connections and
/// drains in-flight requests. Once the server returns, the provided
/// [`ShutdownCoordinator`] executes registered shutdown steps (flushing the
/// database, stopping background services, etc.) in order with per-step
/// timeouts.
pub async fn start(
    state: Arc<AppState>,
    server_config: ServerConfig,
    coordinator: crate::shutdown::ShutdownCoordinator,
) -> Result<(), ServerError> {
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

    // Run ordered shutdown steps (flush DB, stop services, etc.)
    info!("Running shutdown coordinator...");
    coordinator.execute().await;

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

    // Fetch remote pricing (non-blocking, falls back to bundled on failure)
    crate::pricing::refresh_remote_pricing().await;

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

    // Proactively refresh expired OAuth tokens
    refresh_expired_tokens(state, pool, now).await;

    // Backfill subscription tier for OAuth accounts that don't have one yet
    backfill_subscription_tiers(pool).await;

    info!("Startup maintenance complete");
}

/// Clear ALL rate_limited_until entries on startup.
///
/// Rate limit state is ephemeral and should be re-discovered from actual
/// upstream responses. Persisting it across restarts causes stale rate limits
/// (e.g., from a different proxy version) to block accounts that are actually
/// available.
fn clear_expired_rate_limits(pool: &DbPool, _now: i64) -> Result<usize, String> {
    let conn = pool.get().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE accounts SET rate_limited_until = NULL, rate_limit_status = NULL, rate_limit_reset = NULL WHERE rate_limited_until IS NOT NULL OR rate_limit_status IS NOT NULL",
        [],
    )
    .map_err(|e| e.to_string())
}

/// Proactively refresh expired OAuth tokens on startup.
///
/// Loads all OAuth accounts with refresh tokens and attempts to refresh any
/// that have expired or missing access tokens. This prevents the dashboard
/// from showing accounts as "expired" when they have valid refresh tokens.
async fn refresh_expired_tokens(state: &AppState, pool: &DbPool, now: i64) {
    use bccf_database::repositories::account as account_repo;
    use bccf_providers::ProviderRegistry;

    let Some(tm) = state.token_manager::<TokenManager>() else {
        return;
    };
    let Some(registry) = state.provider_registry::<ProviderRegistry>() else {
        return;
    };

    let accounts = match pool.get() {
        Ok(conn) => match account_repo::find_all(&conn) {
            Ok(accs) => accs,
            Err(e) => {
                warn!("Failed to load accounts for token refresh: {e}");
                return;
            }
        },
        Err(e) => {
            warn!("Failed to get DB connection for token refresh: {e}");
            return;
        }
    };

    let mut refreshed = 0u32;
    for account in accounts {
        // Skip accounts without refresh tokens or non-OAuth providers
        if account.refresh_token.is_empty() {
            continue;
        }
        if account.provider != "anthropic" && account.provider != "claude-oauth" && account.provider != "console" {
            continue;
        }
        // Skip if token is still valid
        if let Some(expires_at) = account.expires_at {
            if expires_at > now {
                continue;
            }
        }

        // Token is expired or missing — attempt refresh
        let Some(provider) = registry.get(&account.provider) else {
            continue;
        };

        let refresher = StartupRefresher { provider };
        let persister = StartupPersister { pool };
        let mut account = account;
        match tm.get_valid_access_token(&mut account, &refresher, &persister, now).await {
            Ok(_) => {
                info!(account = %account.name, "Refreshed token on startup");
                refreshed += 1;
            }
            Err(e) => {
                warn!(account = %account.name, error = %e, "Failed to refresh token on startup");
            }
        }
    }

    if refreshed > 0 {
        info!("Refreshed {refreshed} OAuth tokens on startup");
    }
}

/// Fetch subscription tier from Anthropic's profile endpoint for all OAuth
/// accounts that have a valid access token but no subscription tier recorded.
async fn backfill_subscription_tiers(pool: &DbPool) {
    use bccf_database::repositories::account as account_repo;

    const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

    let accounts = match pool.get() {
        Ok(conn) => match account_repo::find_all(&conn) {
            Ok(accs) => accs,
            Err(e) => {
                warn!("backfill_subscription_tiers: failed to load accounts: {e}");
                return;
            }
        },
        Err(e) => {
            warn!("backfill_subscription_tiers: failed to get DB connection: {e}");
            return;
        }
    };

    let client = reqwest::Client::new();
    let mut updated = 0u32;

    for account in accounts {
        // Only claude-oauth accounts with a valid access_token
        if account.provider != "claude-oauth" {
            continue;
        }
        // Skip if both tier and email are already populated
        if account.subscription_tier.is_some() && account.email.is_some() {
            continue;
        }
        let Some(access_token) = &account.access_token else {
            continue;
        };

        let resp = match client
            .get(PROFILE_URL)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(account = %account.name, error = %e, "backfill: profile request failed");
                continue;
            }
        };

        if !resp.status().is_success() {
            warn!(account = %account.name, status = %resp.status(), "backfill: profile endpoint returned error");
            continue;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                warn!(account = %account.name, error = %e, "backfill: failed to parse profile response");
                continue;
            }
        };

        let Ok(conn) = pool.get() else { continue };

        // Backfill subscription tier if missing
        if account.subscription_tier.is_none() {
            let org_type = json["organization"]["organization_type"].as_str();
            let rate_limit_tier = json["organization"]["rate_limit_tier"].as_str();
            if let Some(tier) = build_tier_label(org_type, rate_limit_tier) {
                if account_repo::set_subscription_tier(&conn, &account.id, Some(&tier)).is_ok() {
                    info!(account = %account.name, tier = %tier, "Backfilled subscription tier");
                    updated += 1;
                }
            }
        }

        // Backfill email if missing — try known paths in profile response
        if account.email.is_none() {
            let email = json["account"]["email_address"]
                .as_str()
                .or_else(|| json["email_address"].as_str())
                .or_else(|| json["email"].as_str());
            if let Some(email) = email {
                let _ = account_repo::set_email(&conn, &account.id, Some(email));
                info!(account = %account.name, email = %email, "Backfilled email");
                updated += 1;
            }
        }
    }

    if updated > 0 {
        info!("Backfilled subscription tier/email for {updated} accounts");
    }
}

fn build_tier_label(org_type: Option<&str>, rate_limit_tier: Option<&str>) -> Option<String> {
    let rate_label = rate_limit_tier.and_then(|t| {
        let t = t.strip_prefix("default_claude_").unwrap_or(t);
        let t = t.strip_prefix("claude_").unwrap_or(t);
        if let Some(rest) = t.strip_prefix("max_") {
            return Some(format!("Max {rest}"));
        }
        match t {
            "pro" => Some("Pro".to_string()),
            "max" => Some("Max".to_string()),
            _ if t.contains("max") => Some("Max".to_string()),
            _ => None,
        }
    });

    match org_type {
        Some("claude_team") => Some(
            rate_label
                .map(|r| format!("Team {r}"))
                .unwrap_or_else(|| "Team".to_string()),
        ),
        Some("claude_enterprise") => Some("Enterprise".to_string()),
        Some("claude_pro") => Some("Pro".to_string()),
        Some("claude_max") | Some(_) => rate_label.or_else(|| {
            org_type.map(|o| {
                let s = o.strip_prefix("claude_").unwrap_or(o);
                let mut c = s.chars();
                match c.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
        }),
        None => rate_label,
    }
}

/// Token refresher for startup use.
struct StartupRefresher {
    provider: Arc<dyn bccf_providers::traits::Provider>,
}

#[async_trait::async_trait]
impl TokenRefresher for StartupRefresher {
    async fn refresh_token(
        &self,
        account: &bccf_core::types::Account,
        client_id: &str,
    ) -> Result<bccf_providers::types::TokenRefreshResult, bccf_providers::error::ProviderError> {
        self.provider.refresh_token(account, client_id).await
    }
}

/// Token persister for startup use.
struct StartupPersister<'a> {
    pool: &'a DbPool,
}

impl TokenPersister for StartupPersister<'_> {
    fn persist_tokens(
        &self,
        account_id: &str,
        access_token: &str,
        expires_at: i64,
        refresh_token: &str,
    ) {
        let Ok(conn) = self.pool.get() else { return };
        let _ = conn.execute(
            "UPDATE accounts SET access_token = ?1, expires_at = ?2, refresh_token = ?3 WHERE id = ?4",
            rusqlite::params![access_token, expires_at, refresh_token, account_id],
        );
    }

    fn load_account(&self, account_id: &str) -> Option<bccf_core::types::Account> {
        use bccf_database::repositories::account as account_repo;
        let conn = self.pool.get().ok()?;
        account_repo::find_by_id(&conn, account_id).ok().flatten()
    }
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
