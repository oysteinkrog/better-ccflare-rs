//! Proxy endpoint — the `/v1/messages` hot path.
//!
//! Buffers the incoming request body, selects accounts via the load balancer,
//! tries each in order, forwards the request to the upstream provider,
//! handles rate limits and auth failures with failover, streams responses
//! back to clients, and sends analytics to the post-processor.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use tracing::{debug, error, info, warn};

use bccf_core::AppState;
use bccf_database::repositories::account as account_repo;
use bccf_database::DbPool;
use bccf_load_balancer::{SelectionMeta, SessionStrategy};
use bccf_providers::ProviderRegistry;

use crate::auth::AuthInfo;
use crate::handler::{
    self, detect_agent_from_user_agent, error_response, extract_model_from_body,
    filter_thinking_blocks, get_force_account_id, is_session_bypass, is_thinking_block_error,
    replace_model_in_body, AccountResult, RequestMeta,
};
use crate::post_processor::{PostProcessorHandle, PostProcessorMsg};
use crate::streaming;
use crate::token_manager::TokenManager;

/// Maximum request body size (10 MB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// TTL for cached accounts list (seconds).
const ACCOUNTS_CACHE_TTL_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// Accounts cache — per-AppState to avoid cross-test interference
// ---------------------------------------------------------------------------

/// The cache type stored inside `AppState::accounts_cache`.
type AccountsCacheSlot = Option<(Arc<Vec<bccf_core::types::Account>>, Instant)>;

/// Get accounts, using the per-state cache if fresh enough, otherwise query DB.
/// Returns Arc to avoid cloning 10+ Account structs on every cache hit.
async fn get_accounts_cached(
    pool: &DbPool,
    state: &AppState,
) -> Result<Arc<Vec<bccf_core::types::Account>>, String> {
    // Check cache (std::sync::Mutex — never held across await points)
    {
        let cache = state.accounts_cache.lock().unwrap();
        if let Some((ref accounts, fetched_at)) = *cache {
            if fetched_at.elapsed().as_secs() < ACCOUNTS_CACHE_TTL_SECS {
                return Ok(Arc::clone(accounts));
            }
        }
    }

    // Cache miss — query via spawn_blocking
    let pool_clone = pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = pool_clone.get()?;
        account_repo::find_all(&conn)
    })
    .await;

    match result {
        Ok(Ok(accounts)) => {
            let accounts = Arc::new(accounts);
            let mut cache = state.accounts_cache.lock().unwrap();
            *cache = Some((Arc::clone(&accounts), Instant::now()));
            Ok(accounts)
        }
        Ok(Err(e)) => Err(format!("Failed to load accounts: {e}")),
        Err(e) => Err(format!("Account loading task failed: {e}")),
    }
}

/// Get parsed model mapping for a given model from a JSON mappings string.
fn get_model_mapping(_account_id: &str, mappings_json: &str, model: &str) -> Option<String> {
    let parsed: HashMap<String, String> = serde_json::from_str(mappings_json).ok()?;
    parsed.get(model).cloned()
}

/// Main proxy handler for `/v1/messages` and `/v1/*` routes.
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: axum::http::Request<Body>,
) -> Response {
    let start_time = Instant::now();

    // Extract auth info (set by auth middleware).
    // Defense in depth: the middleware already rejects unauthenticated requests,
    // but we verify here in case the middleware stack is reconfigured.
    let auth_info = req
        .extensions()
        .get::<AuthInfo>()
        .cloned()
        .unwrap_or_default();

    if !auth_info.is_authenticated {
        return error_response(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers = req.headers().clone();

    // Reject path traversal attempts before any forwarding
    if !bccf_core::path_validator::is_safe_request_path(&path) {
        return error_response(StatusCode::BAD_REQUEST, "Invalid request path");
    }

    // Buffer request body
    let body_bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY_SIZE).await {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("Failed to buffer request body: {e}");
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "Request body too large");
        }
    };

    // Build request metadata
    let now = chrono::Utc::now().timestamp_millis();
    let mut meta = RequestMeta::new(&method, &path, &query, now);

    // Detect agent from User-Agent header
    if let Some(ua) = headers.get("user-agent").and_then(|v| v.to_str().ok()) {
        meta.agent_used = detect_agent_from_user_agent(ua);
    }

    // Extract model from body (lightweight — only deserializes the "model" field)
    let requested_model = extract_model_from_body(&body_bytes);

    // Extract project from header
    meta.project = headers
        .get("x-ccflare-project")
        .or_else(|| headers.get("x-better-ccflare-project"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Get database pool
    let Some(pool) = state.db_pool::<DbPool>() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "Database not available");
    };

    // Load accounts (per-state cache — avoids DB query per request)
    let accounts = match get_accounts_cached(pool, &state).await {
        Ok(accs) => accs,
        Err(e) => {
            error!("{e}");
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "Failed to load accounts");
        }
    };

    if accounts.is_empty() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "No accounts configured. Add an account with: better-ccflare --add-account <name> --mode <mode>",
        );
    }

    // Get load balancer
    let load_balancer = state.load_balancer::<SessionStrategy>();

    // Build selection metadata from headers.
    // Proxy-scoped keys must not be able to influence routing decisions:
    // strip force-account and session-bypass hints so they cannot target
    // specific accounts or bypass session limits.
    let is_proxy_key = auth_info.key_scope == Some(bccf_core::types::KeyScope::Proxy);
    let selection_meta = if is_proxy_key {
        SelectionMeta {
            force_account_id: None,
            bypass_session: false,
        }
    } else {
        SelectionMeta {
            force_account_id: get_force_account_id(&headers),
            bypass_session: is_session_bypass(&headers),
        }
    };

    // Build per-account usage map for the load balancer
    let usage_map: HashMap<String, bccf_core::types::RoutingUsageInfo> = {
        if let Some(cache) = state.usage_cache::<bccf_providers::UsageCache>() {
            accounts
                .iter()
                .filter_map(|a| {
                    cache
                        .get(&a.id)?
                        .routing_info()
                        .map(|info| (a.id.clone(), info))
                })
                .collect()
        } else {
            HashMap::new()
        }
    };

    // Select accounts via load balancer
    let (ordered_accounts, session_resets) = if let Some(lb) = load_balancer {
        lb.select(&accounts, &usage_map, &selection_meta, now)
    } else {
        // Fallback: just use accounts as-is sorted by priority
        let mut sorted: Vec<_> = accounts.iter().cloned().collect();
        sorted.sort_by_key(|a| a.priority);
        (sorted, vec![])
    };

    // Persist session resets (fire-and-forget via spawn_blocking, batched)
    if !session_resets.is_empty() {
        let pool_c = pool.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(mut conn) = pool_c.get() {
                // Use a proper transaction so a mid-batch failure rolls back cleanly
                // instead of committing partial updates.
                if let Ok(tx) = conn.transaction() {
                    let mut ok = true;
                    for reset in &session_resets {
                        if tx.execute(
                            "UPDATE accounts SET session_start = ?1, session_request_count = 0 WHERE id = ?2",
                            rusqlite::params![reset.new_session_start, reset.account_id],
                        ).is_err() {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        let _ = tx.commit();
                    }
                    // If not ok, tx is dropped here and the transaction is rolled back.
                }
            }
        });
    }

    if ordered_accounts.is_empty() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "All accounts are paused or rate-limited",
        );
    }

    // Get post-processor handle (if available)
    let post_processor = state.async_writer::<PostProcessorHandle>().cloned();

    // Shared HTTP client — reuses connections across requests (stored in AppState)
    let http_client = state
        .http_client::<reqwest::Client>()
        .cloned()
        .unwrap_or_else(reqwest::Client::new);

    // Try accounts in order (body.clone() is O(1) — Bytes is refcounted)
    let body = body_bytes;
    let result = handler::try_accounts_in_order(
        &ordered_accounts,
        &meta,
        &body,
        |account, req_meta, req_body, attempt| {
            let state = state.clone();
            let post_processor = post_processor.clone();
            let auth_info = auth_info.clone();
            let requested_model = requested_model.clone();
            let client = http_client.clone();
            let base_headers = headers.clone(); // keep as axum HeaderMap

            async move {
                // Get provider for this account
                let provider = state
                    .provider_registry::<ProviderRegistry>()
                    .and_then(|reg| reg.get(&account.provider));

                let Some(provider) = provider else {
                    warn!(
                        account = %account.name,
                        provider = %account.provider,
                        "No provider implementation found"
                    );
                    return AccountResult::Error(format!(
                        "No provider implementation for '{}'",
                        account.provider
                    ));
                };

                // Refresh token if needed (OAuth accounts)
                let mut account = account;
                if let Some(tm) = state.token_manager::<TokenManager>() {
                    let refresher = ProviderRefresher { provider: provider.clone() };
                    let persister = DbPersister { pool: state.db_pool::<DbPool>() };
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    match tm.get_valid_access_token(&mut account, &refresher, &persister, now_ms).await {
                        Ok(token) => {
                            if !token.is_empty() {
                                account.access_token = Some(token);
                            }
                        }
                        Err(e) => {
                            debug!(account = %account.name, error = %e, "Token refresh failed, trying with existing token");
                        }
                    }
                }

                // Build upstream URL
                let url = provider.build_url(&req_meta.path, &req_meta.query, Some(&account));

                // Prepare headers — filter through allowlist first, then let provider
                // add auth headers, then convert to reqwest format.
                let upstream_headers = {
                    let mut h = filter_request_headers(&base_headers);
                    if let Err(e) = provider.prepare_headers(
                        &mut h,
                        account.access_token.as_deref(),
                        account.api_key.as_deref(),
                    ) {
                        warn!(account = %account.name, error = %e, "Failed to prepare auth headers");
                        return AccountResult::AuthFailed(StatusCode::UNAUTHORIZED);
                    }
                    reqwest_headers(&h)
                };

                // SDK clients (no agent detected) get passthrough — no proxy-level
                // model replacement or thinking-block filtering. Agent IDEs
                // (Claude Code, Cursor, Windsurf, Cline) get the full treatment.
                let is_agent_client = req_meta.agent_used.is_some();

                // Transform request body (model mapping, etc.)
                let final_body = match provider.transform_request_body(&req_body, Some(&account)).await {
                    Ok(Some(transformed)) => Bytes::from(transformed),
                    Ok(None) => {
                        // Provider had no mapping — apply proxy-level fallback only for agents
                        if is_agent_client {
                            apply_model_mapping(&account, &requested_model, &req_body)
                        } else {
                            req_body.clone()
                        }
                    }
                    Err(e) => {
                        warn!("Body transform failed: {e}");
                        req_body.clone()
                    }
                };

                // Make upstream request
                let upstream_req = client
                    .request(
                        reqwest::Method::from_bytes(req_meta.method.as_bytes()).unwrap_or(reqwest::Method::POST),
                        &url,
                    )
                    .headers(upstream_headers.clone())
                    .body(final_body.clone())
                    .send()
                    .await;

                let upstream_resp = match upstream_req {
                    Ok(resp) => resp,
                    Err(e) => {
                        error!(account = %account.name, error = %e, "Upstream request failed");
                        return AccountResult::Error(e.to_string());
                    }
                };

                let status = upstream_resp.status();
                let resp_headers = upstream_resp.headers().clone();

                // Handle rate limit (429)
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let rate_info = provider.parse_rate_limit(&axum_headers_from_reqwest(&resp_headers), status.as_u16());
                    // Mark account as rate-limited in DB (fire-and-forget)
                    if let Some(pool) = state.db_pool::<DbPool>() {
                        let pool_c = pool.clone();
                        let until = rate_info
                        .reset_time
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 60_000);
                        let account_id = account.id.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Ok(conn) = pool_c.get() {
                                let _ = conn.execute(
                                    "UPDATE accounts SET rate_limited_until = ?1 WHERE id = ?2",
                                    rusqlite::params![until, account_id],
                                );
                            }
                        });
                    }
                    return AccountResult::RateLimited(rate_info);
                }

                // Handle auth failure (401/403)
                if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                    // Persist an explicit auth-failed marker so dashboard/API status can
                    // surface re-authentication needs instead of showing accounts as valid.
                    if let Some(pool) = state.db_pool::<DbPool>() {
                        let pool_c = pool.clone();
                        let account_id = account.id.clone();
                        let status_label = format!("Auth failed ({})", status.as_u16());
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        tokio::task::spawn_blocking(move || {
                            if let Ok(conn) = pool_c.get() {
                                let _ = conn.execute(
                                    "UPDATE accounts
                                     SET rate_limit_status = ?1,
                                         rate_limit_reset = NULL,
                                         rate_limit_remaining = NULL,
                                         expires_at = CASE WHEN api_key IS NULL THEN ?2 ELSE expires_at END
                                     WHERE id = ?3",
                                    rusqlite::params![status_label, now_ms - 1, account_id],
                                );
                            }
                        });
                    }
                    return AccountResult::AuthFailed(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::UNAUTHORIZED));
                }

                // Handle thinking block error (400 + specific error message)
                // Only attempt thinking-block filtering for agent clients —
                // SDK clients get 400 errors passed through as-is.
                if status == reqwest::StatusCode::BAD_REQUEST {
                    let resp_body = match upstream_resp.bytes().await {
                        Ok(b) => b,
                        Err(e) => return AccountResult::Error(e.to_string()),
                    };
                    if is_agent_client && is_thinking_block_error(StatusCode::BAD_REQUEST, &resp_body) {
                        // Retry with thinking blocks filtered
                        if let Some(filtered) = filter_thinking_blocks(&final_body) {
                            debug!("Retrying with thinking blocks filtered");
                            let retry_resp = client
                                .request(
                                    reqwest::Method::from_bytes(req_meta.method.as_bytes()).unwrap_or(reqwest::Method::POST),
                                    &url,
                                )
                                .headers(upstream_headers)
                                .body(filtered)
                                .send()
                                .await;
                            match retry_resp {
                                Ok(resp) if resp.status().is_success() => {
                                    return build_success_response(
                                        resp,
                                        &provider,
                                        &account,
                                        &req_meta,
                                        &auth_info,
                                        start_time,
                                        attempt,
                                        post_processor.as_ref(),
                                        &state,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                        // Return original error
                        let axum_resp = axum::response::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .body(Body::from(resp_body))
                            .unwrap_or_else(|_| error_response(StatusCode::BAD_REQUEST, "Bad request"));
                        return AccountResult::Success(axum_resp);
                    }
                    // Non-thinking error (or SDK client) — record and return as-is
                    if let Some(pp) = &post_processor {
                        pp.send(PostProcessorMsg::ResponseComplete {
                            request_id: req_meta.id.clone(),
                            account_id: Some(account.id.clone()),
                            path: req_meta.path.clone(),
                            body: resp_body.clone(),
                            response_status: 400,
                            start_time,
                            agent_used: req_meta.agent_used.clone(),
                            project: req_meta.project.clone(),
                            api_key_id: auth_info.api_key_id.clone(),
                            api_key_name: auth_info.api_key_name.clone(),
                            failover_attempts: attempt,
                        });
                    }
                    let axum_resp = axum::response::Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .body(Body::from(resp_body))
                        .unwrap_or_else(|_| error_response(StatusCode::BAD_REQUEST, "Bad request"));
                    return AccountResult::Success(axum_resp);
                }

                // Success (or other non-retryable error)
                build_success_response(
                    upstream_resp,
                    &provider,
                    &account,
                    &req_meta,
                    &auth_info,
                    start_time,
                    attempt,
                    post_processor.as_ref(),
                    &state,
                )
                .await
            }
        },
    )
    .await;

    match result {
        Some((response, succeeded_account_id)) => {
            // Batch stats + rate limit update in a single DB write (fire-and-forget)
            let pool_c = pool.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(conn) = pool_c.get() {
                    let _ = conn.execute(
                        "UPDATE accounts SET request_count = request_count + 1, total_requests = total_requests + 1, session_request_count = session_request_count + 1, last_used = ?1 WHERE id = ?2",
                        rusqlite::params![now, succeeded_account_id],
                    );
                }
            });
            response
        }
        None => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "All accounts failed — request could not be proxied",
        ),
    }
}

// ---------------------------------------------------------------------------
// Model mapping helper
// ---------------------------------------------------------------------------

/// Apply model mappings from account config, returning the (possibly transformed) body.
fn apply_model_mapping(
    account: &bccf_core::types::Account,
    requested_model: &Option<String>,
    req_body: &Bytes,
) -> Bytes {
    let Some(ref mappings_json) = account.model_mappings else {
        return req_body.clone();
    };
    let Some(ref model) = requested_model else {
        return req_body.clone();
    };

    if let Some(mapped) = get_model_mapping(&account.id, mappings_json, model) {
        replace_model_in_body(req_body, model, &mapped).unwrap_or_else(|| req_body.clone())
    } else {
        req_body.clone()
    }
}

/// Build a success response from an upstream reqwest response.
///
/// Handles both streaming (SSE) and non-streaming responses, setting up
/// the tee stream for analytics when streaming.
#[allow(clippy::too_many_arguments)]
async fn build_success_response(
    upstream_resp: reqwest::Response,
    provider: &Arc<dyn bccf_providers::Provider>,
    account: &bccf_core::types::Account,
    req_meta: &RequestMeta,
    auth_info: &AuthInfo,
    start_time: Instant,
    attempt: usize,
    post_processor: Option<&PostProcessorHandle>,
    state: &Arc<AppState>,
) -> AccountResult {
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let axum_resp_headers = axum_headers_from_reqwest(&resp_headers);
    let is_streaming = provider.is_streaming_response(&axum_resp_headers);

    // Parse rate limit info and batch-persist with request stats (fire-and-forget)
    let rate_info = provider.parse_rate_limit(&axum_resp_headers, status.as_u16());
    if let Some(pool) = state.db_pool::<DbPool>() {
        let pool_c = pool.clone();
        let account_id = account.id.clone();
        let status_header = rate_info.status_header.clone();
        let remaining = rate_info.remaining;
        let reset_time = rate_info.reset_time;
        tokio::task::spawn_blocking(move || {
            if let Ok(conn) = pool_c.get() {
                let _ = conn.execute(
                    "UPDATE accounts SET rate_limit_status = ?1, rate_limit_remaining = ?2, rate_limit_reset = ?3 WHERE id = ?4",
                    rusqlite::params![status_header, remaining, reset_time, account_id],
                );
            }
        });
    }

    if is_streaming {
        // Streaming response — tee the stream for analytics
        let byte_stream = upstream_resp.bytes_stream().filter_map(|result| async {
            match result {
                Ok(bytes) => Some(Ok::<Bytes, std::convert::Infallible>(bytes)),
                Err(e) => {
                    warn!("Stream chunk error: {e}");
                    None
                }
            }
        });

        let (tee, analytics_rx) = streaming::tee_stream(byte_stream);

        // Spawn analytics processing
        if let Some(pp) = post_processor {
            let pp = pp.clone();
            let request_id = req_meta.id.clone();
            let account_id = Some(account.id.clone());
            let path = req_meta.path.clone();
            let response_status = status.as_u16();
            let agent_used = req_meta.agent_used.clone();
            let project = req_meta.project.clone();
            let api_key_id = auth_info.api_key_id.clone();
            let api_key_name = auth_info.api_key_name.clone();
            let failover_attempts = attempt;

            tokio::spawn(async move {
                if let Ok(buffer) = analytics_rx.await {
                    pp.send(PostProcessorMsg::StreamComplete {
                        request_id,
                        account_id,
                        path,
                        buffer,
                        response_status,
                        start_time,
                        agent_used,
                        project,
                        api_key_id,
                        api_key_name,
                        failover_attempts,
                    });
                }
            });
        }

        // Build streaming response
        let body = Body::from_stream(tee);
        let mut builder = axum::response::Response::builder()
            .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK));

        // Forward relevant headers
        for (name, value) in &resp_headers {
            if !should_forward_response_header(name.as_str(), is_streaming) {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(name.as_ref()),
                axum::http::HeaderValue::from_bytes(value.as_ref()),
            ) {
                builder = builder.header(name, value);
            }
        }

        match builder.body(body) {
            Ok(resp) => AccountResult::Success(resp),
            Err(e) => AccountResult::Error(format!("Failed to build response: {e}")),
        }
    } else {
        // Non-streaming response
        let resp_body = match upstream_resp.bytes().await {
            Ok(b) => b,
            Err(e) => return AccountResult::Error(format!("Failed to read response: {e}")),
        };

        // Send to post-processor
        if let Some(pp) = post_processor {
            pp.send(PostProcessorMsg::ResponseComplete {
                request_id: req_meta.id.clone(),
                account_id: Some(account.id.clone()),
                path: req_meta.path.clone(),
                body: resp_body.clone(),
                response_status: status.as_u16(),
                start_time,
                agent_used: req_meta.agent_used.clone(),
                project: req_meta.project.clone(),
                api_key_id: auth_info.api_key_id.clone(),
                api_key_name: auth_info.api_key_name.clone(),
                failover_attempts: attempt,
            });
        }

        // Build non-streaming response
        let mut builder = axum::response::Response::builder()
            .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK));

        for (name, value) in &resp_headers {
            if !should_forward_response_header(name.as_str(), is_streaming) {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(name.as_ref()),
                axum::http::HeaderValue::from_bytes(value.as_ref()),
            ) {
                builder = builder.header(name, value);
            }
        }

        match builder.body(Body::from(resp_body)) {
            Ok(resp) => AccountResult::Success(resp),
            Err(e) => AccountResult::Error(format!("Failed to build response: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Header conversion helpers
// ---------------------------------------------------------------------------

/// Filter client request headers through an allowlist before forwarding upstream.
///
/// Only explicitly safe headers pass through. This prevents:
/// - Hop-by-hop headers from being forwarded (RFC 7230 §6.1)
/// - Client IP leakage via X-Forwarded-For, X-Real-IP, X-Forwarded-Host
/// - Internal proxy headers from reaching upstream
/// - Attacker-injected headers from influencing upstream behavior
///
/// Auth headers (authorization, x-api-key) are included in the allowlist because
/// the provider's `prepare_headers()` overwrites them with the correct upstream
/// credentials after filtering.
fn filter_request_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(8);
    for (name, value) in headers {
        if should_forward_request_header(name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// Returns true if a client request header should be forwarded to the upstream API.
///
/// Allowlist approach — everything not listed is stripped.
fn should_forward_request_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    // Exact-match allowlist
    if matches!(
        lower.as_str(),
        "content-type"
            | "content-length"
            | "accept"
            | "accept-encoding"
            | "authorization"
            | "x-api-key"
            | "x-request-id"
            | "user-agent"
    ) {
        return true;
    }

    // Prefix-match allowlist
    if lower.starts_with("anthropic-") || lower.starts_with("x-stainless-") {
        return true;
    }

    false
}

/// Convert axum HeaderMap to reqwest HeaderMap.
fn reqwest_headers(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_ref()),
            reqwest::header::HeaderValue::from_bytes(value.as_ref()),
        ) {
            // Use append instead of insert to preserve multi-value headers (e.g. Set-Cookie).
            out.append(n, v);
        }
    }
    out
}

/// Convert reqwest HeaderMap to axum HeaderMap.
fn axum_headers_from_reqwest(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if let (Ok(n), Ok(v)) = (
            axum::http::HeaderName::from_bytes(name.as_ref()),
            axum::http::HeaderValue::from_bytes(value.as_ref()),
        ) {
            // Use append instead of insert to preserve multi-value headers (e.g. Set-Cookie).
            out.append(n, v);
        }
    }
    out
}

/// Returns true if an upstream response header is safe to forward to the client.
///
/// Uses an allowlist approach: only explicitly permitted headers are forwarded.
/// This prevents upstream servers (including malicious custom endpoints) from
/// injecting headers like `set-cookie`, `www-authenticate`, or `access-control-*`
/// that could override the proxy's own security policies (CORS, auth).
///
/// Framing headers (content-length, transfer-encoding, content-encoding) are always
/// excluded: reqwest auto-decompresses responses, so the original content-length
/// reflects the compressed size while the body we forward is decompressed. Forwarding
/// the stale content-length causes hyper to reset the connection before the full body
/// is sent. Let axum/hyper set the correct framing headers for the outgoing response.
fn should_forward_response_header(name: &str, _is_streaming: bool) -> bool {
    // Core content headers
    if name.eq_ignore_ascii_case("content-type") {
        return true;
    }

    // Caching and retry
    if name.eq_ignore_ascii_case("cache-control")
        || name.eq_ignore_ascii_case("retry-after")
        || name.eq_ignore_ascii_case("expires")
    {
        return true;
    }

    // Request tracing / correlation IDs
    if name.eq_ignore_ascii_case("x-request-id")
        || name.eq_ignore_ascii_case("request-id")
        || name.eq_ignore_ascii_case("x-correlation-id")
    {
        return true;
    }

    // Anthropic-specific headers and rate limit headers
    // Forward any header starting with "anthropic-" so new API headers pass through.
    // Forward "x-ratelimit-*" so clients can observe Anthropic's rate limit state.
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("anthropic-") || lower.starts_with("x-ratelimit-") {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Token refresh adapters
// ---------------------------------------------------------------------------

use crate::token_manager::{TokenPersister, TokenRefresher};
use bccf_core::types::Account;
use bccf_providers::error::ProviderError;
use bccf_providers::traits::Provider as ProviderTrait;
use bccf_providers::types::TokenRefreshResult;

/// Delegates token refresh to the provider.
struct ProviderRefresher {
    provider: Arc<dyn ProviderTrait>,
}

#[async_trait::async_trait]
impl TokenRefresher for ProviderRefresher {
    async fn refresh_token(
        &self,
        account: &Account,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        self.provider.refresh_token(account, client_id).await
    }
}

/// Persists tokens to the SQLite database.
struct DbPersister<'a> {
    pool: Option<&'a DbPool>,
}

impl TokenPersister for DbPersister<'_> {
    fn persist_tokens(
        &self,
        account_id: &str,
        access_token: &str,
        expires_at: i64,
        refresh_token: &str,
    ) {
        let Some(pool) = self.pool else { return };
        let Ok(conn) = pool.get() else { return };
        let _ = conn.execute(
            "UPDATE accounts SET access_token = ?1, expires_at = ?2, refresh_token = ?3 WHERE id = ?4",
            rusqlite::params![access_token, expires_at, refresh_token, account_id],
        );
        info!(account_id = %account_id, "Persisted refreshed token to DB");
    }

    fn persist_subscription_tier(&self, account_id: &str, tier: Option<&str>) {
        let Some(pool) = self.pool else { return };
        let Ok(conn) = pool.get() else { return };
        let _ = account_repo::set_subscription_tier(&conn, account_id, tier);
    }

    fn persist_email(&self, account_id: &str, email: Option<&str>) {
        let Some(pool) = self.pool else { return };
        let Ok(conn) = pool.get() else { return };
        let _ = account_repo::set_email(&conn, account_id, email);
    }

    fn load_account(&self, account_id: &str) -> Option<Account> {
        let pool = self.pool?;
        let conn = pool.get().ok()?;
        account_repo::find_by_id(&conn, account_id).ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_header_is_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let filtered = filter_request_headers(&headers);
        assert!(filtered.get("connection").is_none());
        assert!(filtered.get("content-type").is_some());
    }

    #[test]
    fn hop_by_hop_headers_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("proxy-authenticate", "Basic".parse().unwrap());
        headers.insert("proxy-authorization", "Basic dGVzdA==".parse().unwrap());
        headers.insert("te", "trailers".parse().unwrap());
        headers.insert("trailer", "Expires".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let filtered = filter_request_headers(&headers);
        assert!(filtered.get("connection").is_none());
        assert!(filtered.get("keep-alive").is_none());
        assert!(filtered.get("transfer-encoding").is_none());
        assert!(filtered.get("upgrade").is_none());
        assert!(filtered.get("proxy-authenticate").is_none());
        assert!(filtered.get("proxy-authorization").is_none());
        assert!(filtered.get("te").is_none());
        assert!(filtered.get("trailer").is_none());
        assert_eq!(filtered.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn custom_attacker_headers_not_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert("x-evil", "1".parse().unwrap());
        headers.insert("x-internal-secret", "leak".parse().unwrap());
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        headers.insert("x-real-ip", "1.2.3.4".parse().unwrap());
        headers.insert("x-forwarded-host", "attacker.com".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let filtered = filter_request_headers(&headers);
        assert!(filtered.get("x-evil").is_none());
        assert!(filtered.get("x-internal-secret").is_none());
        assert!(filtered.get("x-forwarded-for").is_none());
        assert!(filtered.get("x-real-ip").is_none());
        assert!(filtered.get("x-forwarded-host").is_none());
        assert_eq!(filtered.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn anthropic_beta_header_passes_through() {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", "messages-2024-04-04".parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let filtered = filter_request_headers(&headers);
        assert_eq!(
            filtered.get("anthropic-beta").unwrap(),
            "messages-2024-04-04"
        );
        assert_eq!(
            filtered.get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        assert_eq!(filtered.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn allowlisted_headers_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("content-length", "42".parse().unwrap());
        headers.insert("accept", "*/*".parse().unwrap());
        headers.insert("accept-encoding", "gzip".parse().unwrap());
        headers.insert("authorization", "Bearer test".parse().unwrap());
        headers.insert("x-api-key", "sk-test".parse().unwrap());
        headers.insert("x-request-id", "req-123".parse().unwrap());
        headers.insert("user-agent", "test/1.0".parse().unwrap());
        headers.insert("x-stainless-lang", "rust".parse().unwrap());

        let filtered = filter_request_headers(&headers);
        assert_eq!(filtered.len(), 9);
        assert_eq!(filtered.get("content-type").unwrap(), "application/json");
        assert_eq!(filtered.get("content-length").unwrap(), "42");
        assert_eq!(filtered.get("accept").unwrap(), "*/*");
        assert_eq!(filtered.get("accept-encoding").unwrap(), "gzip");
        assert_eq!(filtered.get("authorization").unwrap(), "Bearer test");
        assert_eq!(filtered.get("x-api-key").unwrap(), "sk-test");
        assert_eq!(filtered.get("x-request-id").unwrap(), "req-123");
        assert_eq!(filtered.get("user-agent").unwrap(), "test/1.0");
        assert_eq!(filtered.get("x-stainless-lang").unwrap(), "rust");
    }

    #[test]
    fn response_hop_by_hop_stripped() {
        assert!(!should_forward_response_header("connection", false));
        assert!(!should_forward_response_header("keep-alive", false));
        assert!(!should_forward_response_header("proxy-authenticate", false));
        assert!(!should_forward_response_header("proxy-authorization", false));
        assert!(!should_forward_response_header("te", false));
        assert!(!should_forward_response_header("trailer", false));
        assert!(!should_forward_response_header("upgrade", false));
        assert!(!should_forward_response_header("transfer-encoding", false));
        assert!(!should_forward_response_header("content-length", false));
        // Safe headers pass through
        assert!(should_forward_response_header("content-type", false));
        assert!(should_forward_response_header("x-ratelimit-remaining", false));
    }
}
