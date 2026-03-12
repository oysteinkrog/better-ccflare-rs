//! End-to-end integration tests for the Rust HTTP server API endpoints.
//!
//! These tests use axum's test helpers (via tower::ServiceExt::oneshot) to
//! send requests directly to the router without a real TCP listener, avoiding
//! port conflicts and network dependencies.
//!
//! Run with: `cargo test -p bccf-proxy --test api_tests`
//!
//! Note: These live in the proxy crate's tests/ directory but are standalone
//! integration tests that don't require the `integration` feature flag.

use std::sync::Arc;

use axum::body::Body;
use axum::routing::{delete, get, post};
use axum::Router;
use http::Request;
use serde_json::Value;
use tower::ServiceExt;

use bccf_core::config::Config;
use bccf_core::state::AppStateBuilder;
use bccf_core::types::{Account, ProxyRequest};
use bccf_database::pool::{create_memory_pool, PoolConfig};
use bccf_database::repositories::{account as account_repo, request as request_repo};
use bccf_database::DbPool;

// ---------------------------------------------------------------------------
// Test harness — builds a minimal router (no dashboard dependency)
// ---------------------------------------------------------------------------

fn build_test_router(state: Arc<bccf_core::AppState>) -> Router {
    use bccf_proxy::{accounts, api, handlers};

    Router::new()
        .route("/health", get(api::health))
        .route("/api/version", get(api::version))
        .route("/api/system/info", get(api::system_info))
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
        .route("/api/requests", get(handlers::requests::list_requests))
        .route(
            "/api/requests/{id}/payload",
            get(handlers::requests::get_request_payload),
        )
        .route(
            "/api/requests/stream",
            get(handlers::streams::request_events_stream),
        )
        .route("/api/logs/stream", get(handlers::logs::logs_stream))
        .route("/api/stats", get(handlers::stats::get_stats))
        .route("/api/analytics", get(handlers::analytics::get_analytics))
        .route("/api/logs", get(handlers::logs::logs_history))
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
        .fallback(api::not_found)
        .with_state(state)
}

fn setup() -> Router {
    let pool = create_memory_pool(&PoolConfig::default()).expect("Failed to create test DB pool");
    seed_test_data(&pool);

    let config = Config::load(Some(std::path::PathBuf::from(
        "/tmp/bccf-integ-test-nonexistent/config.json",
    )))
    .unwrap();

    let state = Arc::new(AppStateBuilder::new(config).db_pool(pool).build());
    build_test_router(state)
}

/// Setup with the full server router (includes proxy routes, API keys, agents).
fn setup_with_proxy() -> Router {
    let pool = create_memory_pool(&PoolConfig::default()).expect("Failed to create test DB pool");
    seed_test_data(&pool);

    let config = Config::load(Some(std::path::PathBuf::from(
        "/tmp/bccf-integ-test-nonexistent/config.json",
    )))
    .unwrap();

    let state = Arc::new(AppStateBuilder::new(config).db_pool(pool).build());
    bccf_proxy::server::build_router(state)
}

fn seed_test_data(pool: &DbPool) {
    let conn = pool.get().unwrap();

    let acc1 = Account {
        id: "acc-1".to_string(),
        name: "Test Account 1".to_string(),
        provider: "claude-oauth".to_string(),
        api_key: None,
        refresh_token: "rt_test1".to_string(),
        access_token: Some("at_test1".to_string()),
        expires_at: Some(9999999999999),
        request_count: 5,
        total_requests: 10,
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

    let acc2 = Account {
        id: "acc-2".to_string(),
        name: "Test Account 2".to_string(),
        provider: "zai".to_string(),
        api_key: Some("sk-test".to_string()),
        refresh_token: String::new(),
        access_token: None,
        expires_at: None,
        request_count: 3,
        total_requests: 5,
        last_used: None,
        created_at: 1700000000000,
        rate_limited_until: None,
        session_start: None,
        session_request_count: 0,
        paused: false,
        rate_limit_reset: None,
        rate_limit_status: None,
        rate_limit_remaining: None,
        priority: 1,
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
        overage_protection: true,
    };

    account_repo::create(&conn, &acc1).unwrap();
    account_repo::create(&conn, &acc2).unwrap();

    for i in 0..10 {
        let req = ProxyRequest {
            id: format!("req-{i}"),
            timestamp: 1700000000000 + i * 60_000,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            account_used: Some(if i % 2 == 0 {
                "acc-1".to_string()
            } else {
                "acc-2".to_string()
            }),
            status_code: Some(if i < 8 { 200 } else { 500 }),
            success: i < 8,
            error_message: if i >= 8 {
                Some("rate limited".to_string())
            } else {
                None
            },
            response_time_ms: Some(100 + i * 10),
            failover_attempts: 0,
            model: Some(if i % 3 == 0 {
                "claude-3-opus".to_string()
            } else {
                "claude-sonnet-4-5-20250929".to_string()
            }),
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cost_usd: Some(0.01),
            input_tokens: Some(100),
            cache_read_input_tokens: Some(20),
            cache_creation_input_tokens: None,
            output_tokens: Some(50),
            agent_used: Some("test-agent".to_string()),
            tokens_per_second: Some(33.3),
            project: Some("test-project".to_string()),
            api_key_id: None,
            api_key_name: None,
        };
        request_repo::save(&conn, &req).unwrap();
    }

    request_repo::save_payload(
        &conn,
        "req-0",
        Some(r#"{"model":"claude-3-opus","messages":[{"role":"user","content":"hello"}]}"#),
        Some(r#"{"id":"msg-1","content":[{"type":"text","text":"Hi!"}]}"#),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Helper to make requests and parse JSON
// ---------------------------------------------------------------------------

async fn get_json(app: Router, uri: &str) -> (u16, Value) {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn post_json(app: Router, uri: &str, body: &Value) -> (u16, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

// ===========================================================================
// Tests
// ===========================================================================

// --- Health & system info ---

#[tokio::test]
async fn health_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/health").await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(body.as_object().map(|o| o.len()), Some(1));
}

#[tokio::test]
async fn version_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/api/version").await;
    assert_eq!(status, 200);
    assert!(body.get("version").is_some());
}

#[tokio::test]
async fn system_info_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/api/system/info").await;
    assert_eq!(status, 200);
    assert!(body.get("platform").is_some());
    assert!(body.get("arch").is_some());
}

// --- Config ---

#[tokio::test]
async fn config_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/api/config").await;
    assert_eq!(status, 200);
    assert!(body.get("port").is_some());
    assert!(body.get("lb_strategy").is_some());
}

#[tokio::test]
async fn strategies_returns_list() {
    let app = setup();
    let (status, body) = get_json(app, "/api/config/strategies").await;
    assert_eq!(status, 200);
    assert!(body["strategies"].as_array().unwrap().len() >= 4);
}

#[tokio::test]
async fn set_strategy_valid() {
    let app = setup();
    let (status, body) = post_json(
        app,
        "/api/config/strategy",
        &serde_json::json!({"strategy": "session"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn set_strategy_invalid() {
    let app = setup();
    let (status, body) = post_json(
        app,
        "/api/config/strategy",
        &serde_json::json!({"strategy": "nonexistent"}),
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.get("error").is_some());
}

#[tokio::test]
async fn retention_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/api/config/retention").await;
    assert_eq!(status, 200);
    assert!(body.get("payloadDays").is_some());
    assert!(body.get("requestDays").is_some());
}

// --- Accounts ---

#[tokio::test]
async fn list_accounts_returns_array() {
    let app = setup();
    let (status, body) = get_json(app, "/api/accounts").await;
    assert_eq!(status, 200);
    let accounts = body.as_array().unwrap();
    assert!(accounts.len() >= 2);
    assert!(accounts[0].get("id").is_some());
    assert!(accounts[0].get("name").is_some());
    assert!(accounts[0].get("provider").is_some());
}

#[tokio::test]
async fn pause_account() {
    let app = setup();
    let (status, body) = post_json(app, "/api/accounts/acc-1/pause", &serde_json::json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn resume_account() {
    let app = setup();
    let (status, body) = post_json(app, "/api/accounts/acc-1/resume", &serde_json::json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn update_priority() {
    let app = setup();
    let (status, _) = post_json(
        app,
        "/api/accounts/acc-1/priority",
        &serde_json::json!({"priority": 5}),
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn nonexistent_account_returns_404() {
    let app = setup();
    let (status, _) = post_json(
        app,
        "/api/accounts/nonexistent/pause",
        &serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 404);
}

// --- Request history ---

#[tokio::test]
async fn requests_returns_paginated() {
    let app = setup();
    let (status, body) = get_json(app, "/api/requests?page=1&limit=5").await;
    assert_eq!(status, 200);

    let requests = body.as_array().unwrap();
    assert_eq!(requests.len(), 5);

    let req = &requests[0];
    assert!(req.get("id").is_some());
    assert!(req.get("timestamp").is_some());
    assert!(req.get("method").is_some());
    assert!(req.get("model").is_some());
}

#[tokio::test]
async fn requests_page_2() {
    let app = setup();
    let (status, body) = get_json(app, "/api/requests?page=2&limit=5").await;
    assert_eq!(status, 200);
    assert_eq!(body.as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn request_payload_found() {
    let app = setup();
    let (status, body) = get_json(app, "/api/requests/req-0/payload").await;
    assert_eq!(status, 200);
    assert_eq!(body["requestId"], "req-0");
    assert!(body.get("requestBody").is_some());
    assert!(body.get("responseBody").is_some());
    assert_eq!(body["meta"]["requestBodyTruncated"], false);
}

#[tokio::test]
async fn request_payload_not_found() {
    let app = setup();
    let (status, _) = get_json(app, "/api/requests/nonexistent/payload").await;
    assert_eq!(status, 404);
}

// --- Stats ---

#[tokio::test]
async fn stats_returns_aggregated() {
    let app = setup();
    let (status, body) = get_json(app, "/api/stats").await;
    assert_eq!(status, 200);
    assert_eq!(body["totalRequests"], 10);
    assert!(body["successRate"].as_f64().unwrap() > 0.0);
    assert!(body["activeAccounts"].as_i64().unwrap() >= 2);
    assert!(body["totalTokens"].as_i64().unwrap() > 0);
    assert!(body["topModels"].as_array().unwrap().len() >= 2);
    assert!(body["accounts"].as_array().unwrap().len() >= 2);
}

// --- Analytics ---

#[tokio::test]
async fn analytics_returns_structured() {
    let app = setup();
    let (status, body) = get_json(app, "/api/analytics?range=30d").await;
    assert_eq!(status, 200);

    assert!(body.get("meta").is_some());
    assert!(body.get("totals").is_some());
    assert!(body.get("timeSeries").is_some());
    assert!(body.get("tokenBreakdown").is_some());
    assert!(body.get("modelDistribution").is_some());
    assert!(body.get("accountPerformance").is_some());
    assert!(body.get("costByModel").is_some());
    assert!(body.get("modelPerformance").is_some());

    assert_eq!(body["meta"]["range"], "30d");
    assert_eq!(body["meta"]["bucket"], "1d");
    assert_eq!(body["meta"]["cumulative"], false);
}

#[tokio::test]
async fn analytics_cumulative() {
    let app = setup();
    let (status, body) = get_json(app, "/api/analytics?range=30d&mode=cumulative").await;
    assert_eq!(status, 200);
    assert_eq!(body["meta"]["cumulative"], true);
}

// --- Token health ---

#[tokio::test]
async fn token_health_returns_report() {
    let app = setup();
    let (status, body) = get_json(app, "/api/token-health").await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    let data = &body["data"];
    assert!(data.get("accounts").is_some());
    assert!(data.get("summary").is_some());
    assert_eq!(data["summary"]["total"], 2);
}

#[tokio::test]
async fn token_health_reauth() {
    let app = setup();
    let (status, body) = get_json(app, "/api/token-health/reauth").await;
    assert_eq!(status, 200);
    assert!(body.get("accounts").is_some());
    assert!(body.get("count").is_some());
    assert!(body.get("needsReauth").is_some());
}

#[tokio::test]
async fn token_health_by_name() {
    let app = setup();
    let (status, body) = get_json(app, "/api/token-health/Test%20Account%201").await;
    assert_eq!(status, 200);
    assert_eq!(body["accountName"], "Test Account 1");
}

#[tokio::test]
async fn token_health_by_name_not_found() {
    let app = setup();
    let (status, _) = get_json(app, "/api/token-health/nonexistent").await;
    assert_eq!(status, 404);
}

// --- Logs ---

#[tokio::test]
async fn logs_history_returns_200() {
    let app = setup();
    let (status, body) = get_json(app, "/api/logs").await;
    assert_eq!(status, 200);
    assert!(body.get("logs").is_some());
}

// --- SSE streams ---

#[tokio::test]
async fn request_stream_returns_sse() {
    let app = setup();
    let req = Request::builder()
        .uri("/api/requests/stream")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
}

#[tokio::test]
async fn logs_stream_returns_sse() {
    let app = setup();
    let req = Request::builder()
        .uri("/api/logs/stream")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "text/event-stream"
    );
}

// --- 404 fallback ---

#[tokio::test]
async fn unknown_route_returns_404() {
    let app = setup();
    let (status, body) = get_json(app, "/api/nonexistent").await;
    assert_eq!(status, 404);
    assert!(body.get("error").is_some());
}

// --- Proxy route completeness ---

/// POST /v1/messages should not return 404 (it's wired up).
/// It will return 503 "no available accounts" since there's no provider
/// registry, but the route itself exists.
#[tokio::test]
async fn proxy_v1_messages_returns_non_404() {
    let app = setup_with_proxy();
    let (status, _body) = post_json(
        app,
        "/v1/messages",
        &serde_json::json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [{"role": "user", "content": "test"}],
            "max_tokens": 10
        }),
    )
    .await;
    // Should NOT be 404. Likely 503 (no provider registry / no accounts) or 401.
    assert_ne!(status, 404, "POST /v1/messages must be a registered route");
}

/// Verify all expected routes are registered (none return 404).
#[tokio::test]
async fn all_expected_routes_registered() {
    let get_routes = vec![
        "/health",
        "/api/version",
        "/api/system/info",
        "/api/config",
        "/api/config/strategy",
        "/api/config/strategies",
        "/api/config/retention",
        "/api/accounts",
        "/api/requests",
        "/api/requests/stream",
        "/api/logs/stream",
        "/api/stats",
        "/api/analytics",
        "/api/logs",
        "/api/token-health",
        "/api/token-health/reauth",
        "/api/keys",
        "/api/keys/stats",
        "/api/agents",
    ];

    for route in get_routes {
        let app = setup_with_proxy();
        let req = Request::builder().uri(route).body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(
            resp.status().as_u16(),
            404,
            "GET {route} should be registered (got 404)"
        );
    }
}
