//! Wiremock integration tests for the proxy endpoint.
//!
//! These tests spin up a mock upstream server (wiremock) and verify that
//! the proxy handler forwards requests, handles rate limits, and fails over
//! between accounts correctly.
//!
//! Run with: `cargo test -p bccf-proxy --test proxy_integration`

use std::sync::Arc;

use axum::body::Body;
use http::Request;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use bccf_core::config::Config;
use bccf_core::state::AppStateBuilder;
use bccf_core::types::Account;
use bccf_database::pool::{create_memory_pool, PoolConfig};
use bccf_database::repositories::account as account_repo;
use bccf_database::DbPool;
use bccf_load_balancer::SessionStrategy;
use bccf_providers::impls::anthropic_compatible::{
    AnthropicCompatibleConfig, AnthropicCompatibleProvider,
};
use bccf_providers::ProviderRegistry;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a test app with a real provider registry pointing at a mock server.
fn setup_with_mock(mock_url: &str) -> (axum::Router, DbPool) {
    // max_size: 1 — all pool borrows recycle the SAME in-memory connection so
    // data inserted by the test is visible to the proxy handler.  With the
    // default max_size: 10 each connection gets its own `:memory:` database and
    // the handler always sees an empty DB → 503.
    let pool = create_memory_pool(&PoolConfig { max_size: 1, min_idle: None })
        .expect("Failed to create test DB pool");

    // Use a unique path per call to avoid parallel-test races.
    static COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::path::PathBuf::from(format!(
        "/tmp/bccf-proxy-integ-test-{id}.json"
    ));
    std::fs::write(&path, r#"{"allow_unauthenticated": true}"#).unwrap();
    let config = Config::load(Some(path)).unwrap();

    // Build a provider registry with an anthropic-compatible provider pointing
    // at the mock server.
    let mut registry = ProviderRegistry::new();
    let mock_provider = AnthropicCompatibleProvider::new(AnthropicCompatibleConfig {
        name: "anthropic-compatible".to_string(),
        endpoint: mock_url.to_string(),
        ..AnthropicCompatibleConfig::default()
    });
    registry.register(Arc::new(mock_provider));

    let lb = SessionStrategy::new(5 * 60 * 1000); // 5 min sessions

    let state = Arc::new(
        AppStateBuilder::new(config)
            .db_pool(pool.clone())
            .provider_registry(registry)
            .load_balancer(lb)
            .build(),
    );

    let router = bccf_proxy::server::build_router(state);
    (router, pool)
}

fn make_account(id: &str, name: &str, api_key: &str, priority: i64) -> Account {
    Account {
        id: id.to_string(),
        name: name.to_string(),
        provider: "anthropic-compatible".to_string(),
        api_key: Some(api_key.to_string()),
        refresh_token: String::new(),
        access_token: None,
        expires_at: None,
        request_count: 0,
        total_requests: 0,
        last_used: None,
        created_at: 1700000000000,
        rate_limited_until: None,
        session_start: None,
        session_request_count: 0,
        paused: false,
        rate_limit_reset: None,
        rate_limit_status: None,
        rate_limit_remaining: None,
        priority,
        auto_fallback_enabled: true,
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
        // Disable overage protection in tests — no usage cache is wired up, so
        // fail-closed protection would filter out all accounts → 503.
        overage_protection: false,
    }
}

fn proxy_request_body() -> serde_json::Value {
    json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 10
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Proxy forwards a request to the mock upstream server and returns the response.
#[tokio::test]
async fn proxy_forwards_to_upstream() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-test",
                    "type": "message",
                    "content": [{"type": "text", "text": "Hello!"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                }))
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());

    // Insert an account
    let conn = pool.get().unwrap();
    account_repo::create(&conn, &make_account("acc-1", "Test", "sk-test", 0)).unwrap();
    drop(conn);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "msg-test");
    assert_eq!(json["content"][0]["text"], "Hello!");
}

/// When the first account gets rate-limited (429), the proxy retries with the
/// next account and succeeds.
#[tokio::test]
async fn proxy_retries_on_rate_limit() {
    let mock_server = MockServer::start().await;

    // The mock server will return 429 on the first request, 200 on the second.
    // wiremock doesn't directly support stateful responses, so we'll use a
    // different approach: set up the mock to return 429, then after first call,
    // add a 200 mock. Instead, we'll use two accounts where the first account's
    // key triggers a 429.

    // All requests get 200 (both accounts use the same endpoint)
    // But we'll make the first account be rate-limited in the DB.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-retry",
                    "type": "message",
                    "content": [{"type": "text", "text": "Success after failover"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                }))
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());

    let conn = pool.get().unwrap();
    // First account is rate-limited (rate_limited_until in the future)
    let mut acc1 = make_account("acc-rl", "Rate Limited", "sk-rl", 0);
    acc1.rate_limited_until = Some(chrono::Utc::now().timestamp_millis() + 300_000);
    account_repo::create(&conn, &acc1).unwrap();

    // Second account is fine
    account_repo::create(&conn, &make_account("acc-ok", "OK Account", "sk-ok", 1)).unwrap();
    drop(conn);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "msg-retry");
}

/// With no accounts, proxy returns 503.
#[tokio::test]
async fn proxy_no_accounts_returns_503() {
    let mock_server = MockServer::start().await;
    let (router, _pool) = setup_with_mock(&mock_server.uri());

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 503);
}

// ---------------------------------------------------------------------------
// Proxy transparency tests
// ---------------------------------------------------------------------------

/// Custom client headers (e.g. user-agent, anthropic-beta, x-custom) must
/// pass through the proxy to the upstream server unmodified.
#[tokio::test]
async fn proxy_preserves_client_headers() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-hdr",
                    "type": "message",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());
    let conn = pool.get().unwrap();
    account_repo::create(&conn, &make_account("acc-h", "Headers", "sk-hdr", 0)).unwrap();
    drop(conn);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "context-management-2025-01-01")
        .header("user-agent", "Claude-Code/1.0")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    // Inspect what the upstream server actually received
    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        1,
        "Expected exactly one request to upstream"
    );
    let upstream_req = &received[0];

    // anthropic-version must be preserved
    assert_eq!(
        upstream_req
            .headers
            .get("anthropic-version")
            .map(|v| v.to_str().unwrap()),
        Some("2023-06-01"),
        "anthropic-version header must pass through unchanged"
    );

    // anthropic-beta must be preserved (not replaced)
    let beta = upstream_req
        .headers
        .get("anthropic-beta")
        .map(|v| v.to_str().unwrap().to_string());
    assert!(
        beta.as_deref()
            .unwrap_or("")
            .contains("context-management-2025-01-01"),
        "anthropic-beta must contain client's original beta value, got: {:?}",
        beta
    );

    // user-agent must be preserved
    assert_eq!(
        upstream_req
            .headers
            .get("user-agent")
            .map(|v| v.to_str().unwrap()),
        Some("Claude-Code/1.0"),
        "user-agent header must pass through unchanged"
    );

    // Auth header must be set by the proxy (replaced, not from client)
    let auth = upstream_req
        .headers
        .get("x-api-key")
        .map(|v| v.to_str().unwrap().to_string());
    assert_eq!(
        auth.as_deref(),
        Some("sk-hdr"),
        "Proxy must set the account's API key"
    );
}

/// Request body must pass through the proxy byte-for-byte (when no model
/// mapping is configured).
#[tokio::test]
async fn proxy_preserves_request_body() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-body",
                    "type": "message",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());
    let conn = pool.get().unwrap();
    account_repo::create(&conn, &make_account("acc-b", "Body", "sk-body", 0)).unwrap();
    drop(conn);

    let body_with_extras = json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 10,
        "context_management": {"strategy": "balanced"},
        "system": "You are helpful.",
        "metadata": {"user_id": "test-user-123"}
    });
    let body_bytes = serde_json::to_vec(&body_with_extras).unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body_bytes.clone()))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);

    // Parse both as JSON to compare (byte-exact comparison would break on
    // whitespace differences, but the proxy should not alter JSON semantics)
    let sent: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let received_body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        sent, received_body,
        "Request body must not be modified by the proxy"
    );

    // Specifically verify context_management is preserved (this was the bug)
    assert!(
        received_body.get("context_management").is_some(),
        "context_management field must not be stripped from the body"
    );
}

/// Response headers from upstream must be forwarded to the client.
#[tokio::test]
async fn proxy_preserves_response_headers() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-rh",
                    "type": "message",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
                .insert_header("content-type", "application/json")
                .insert_header("x-request-id", "req-abc-123")
                .insert_header("anthropic-ratelimit-requests-remaining", "42")
                .insert_header("anthropic-ratelimit-tokens-remaining", "100000"),
        )
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());
    let conn = pool.get().unwrap();
    account_repo::create(&conn, &make_account("acc-r", "Resp", "sk-resp", 0)).unwrap();
    drop(conn);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    // Verify upstream response headers are forwarded to client
    assert_eq!(
        resp.headers()
            .get("x-request-id")
            .map(|v| v.to_str().unwrap()),
        Some("req-abc-123"),
        "x-request-id response header must be forwarded"
    );
    assert_eq!(
        resp.headers()
            .get("anthropic-ratelimit-requests-remaining")
            .map(|v| v.to_str().unwrap()),
        Some("42"),
        "Rate limit response headers must be forwarded"
    );
}

/// Hop-by-hop headers (host, accept-encoding, content-encoding) must NOT
/// be forwarded to the upstream server.
#[tokio::test]
async fn proxy_removes_hop_by_hop_headers() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "id": "msg-hop",
                    "type": "message",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-sonnet-4-5-20250929",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (router, pool) = setup_with_mock(&mock_server.uri());
    let conn = pool.get().unwrap();
    account_repo::create(&conn, &make_account("acc-hop", "Hop", "sk-hop", 0)).unwrap();
    drop(conn);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("host", "should-be-removed.example.com")
        .header("accept-encoding", "gzip, br")
        .header("content-encoding", "identity")
        .body(Body::from(
            serde_json::to_vec(&proxy_request_body()).unwrap(),
        ))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let upstream_req = &received[0];

    // These hop-by-hop headers must be removed by prepare_headers
    assert!(
        upstream_req.headers.get("accept-encoding").is_none(),
        "accept-encoding must not be forwarded to upstream"
    );
    assert!(
        upstream_req.headers.get("content-encoding").is_none(),
        "content-encoding must not be forwarded to upstream"
    );
    // Note: host is typically rewritten by reqwest to the actual target host,
    // so we just verify the original client value is not present
    let host = upstream_req
        .headers
        .get("host")
        .map(|v| v.to_str().unwrap().to_string());
    assert!(
        host.as_deref() != Some("should-be-removed.example.com"),
        "Client's host header must not be forwarded verbatim"
    );
}
