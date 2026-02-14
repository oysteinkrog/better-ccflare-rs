//! Proxy endpoint — the `/v1/messages` hot path.
//!
//! Buffers the incoming request body, selects accounts via the load balancer,
//! tries each in order, forwards the request to the upstream provider,
//! handles rate limits and auth failures with failover, streams responses
//! back to clients, and sends analytics to the post-processor.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use tracing::{debug, error, warn};

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

/// Maximum request body size (10 MB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Main proxy handler for `/v1/messages` and `/v1/*` routes.
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: axum::http::Request<Body>,
) -> Response {
    let start_time = Instant::now();

    // Extract auth info (set by auth middleware)
    let auth_info = req
        .extensions()
        .get::<AuthInfo>()
        .cloned()
        .unwrap_or_default();

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers = req.headers().clone();

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

    // Extract model from body
    let requested_model = extract_model_from_body(&body_bytes);

    // Extract project from header
    meta.project = headers
        .get("x-ccflare-project")
        .or_else(|| headers.get("x-better-ccflare-project"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Get database pool
    let Some(pool) = state.db_pool::<DbPool>() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Database not available",
        );
    };

    // Load accounts from DB
    let accounts = {
        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to get DB connection: {e}");
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Database connection failed",
                );
            }
        };
        match account_repo::find_all(&conn) {
            Ok(accs) => accs,
            Err(e) => {
                error!("Failed to load accounts: {e}");
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Failed to load accounts",
                );
            }
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

    // Build selection metadata from headers
    let selection_meta = SelectionMeta {
        force_account_id: get_force_account_id(&headers),
        bypass_session: is_session_bypass(&headers),
    };

    // Select accounts via load balancer
    let (ordered_accounts, session_resets) = if let Some(lb) = load_balancer {
        lb.select(&accounts, &selection_meta, now)
    } else {
        // Fallback: just use accounts as-is sorted by priority
        let mut sorted: Vec<_> = accounts.iter().cloned().collect();
        sorted.sort_by_key(|a| a.priority);
        (sorted, vec![])
    };

    // Persist session resets
    if !session_resets.is_empty() {
        if let Ok(conn) = pool.get() {
            for reset in &session_resets {
                let _ = conn.execute(
                    "UPDATE accounts SET session_start = ?1, session_request_count = 0 WHERE id = ?2",
                    rusqlite::params![reset.new_session_start, reset.account_id],
                );
            }
        }
    }

    if ordered_accounts.is_empty() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "All accounts are paused or rate-limited",
        );
    }

    // Get post-processor handle (if available)
    let post_processor = state.async_writer::<PostProcessorHandle>().cloned();

    // Try accounts in order
    let body = Bytes::from(body_bytes.to_vec());
    let result = handler::try_accounts_in_order(
        &ordered_accounts,
        &meta,
        &body,
        |account, req_meta, req_body, attempt| {
            let state = state.clone();
            let headers = headers.clone();
            let post_processor = post_processor.clone();
            let auth_info = auth_info.clone();
            let requested_model = requested_model.clone();
            let start_time = start_time;

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

                // Build upstream URL
                let url = provider.build_url(&req_meta.path, &req_meta.query, Some(&account));

                // Prepare headers
                let mut upstream_headers = headers.clone();
                provider.prepare_headers(
                    &mut upstream_headers,
                    account.access_token.as_deref(),
                    account.api_key.as_deref(),
                );

                // Transform request body (model mapping, etc.)
                let final_body = match provider.transform_request_body(&req_body, Some(&account)).await {
                    Ok(Some(transformed)) => Bytes::from(transformed),
                    Ok(None) => {
                        // Apply model mappings from account config if present
                        if let Some(ref mappings_json) = account.model_mappings {
                            if let Some(ref model) = requested_model {
                                if let Ok(mappings) = serde_json::from_str::<serde_json::Value>(mappings_json) {
                                    if let Some(mapped) = mappings.get(model).and_then(|v| v.as_str()) {
                                        if let Some(replaced) = replace_model_in_body(&req_body, mapped) {
                                            replaced
                                        } else {
                                            req_body.clone()
                                        }
                                    } else {
                                        req_body.clone()
                                    }
                                } else {
                                    req_body.clone()
                                }
                            } else {
                                req_body.clone()
                            }
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
                let client = reqwest::Client::new();
                let upstream_req = client
                    .request(
                        reqwest::Method::from_bytes(req_meta.method.as_bytes()).unwrap_or(reqwest::Method::POST),
                        &url,
                    )
                    .headers(reqwest_headers(&upstream_headers))
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
                    let rate_info = provider.parse_rate_limit(&axum_headers(&resp_headers), status.as_u16());
                    // Mark account as rate-limited in DB
                    if let Some(pool) = state.db_pool::<DbPool>() {
                        if let Ok(conn) = pool.get() {
                            let until = rate_info.reset_time.unwrap_or(now + 60_000);
                            let _ = conn.execute(
                                "UPDATE accounts SET rate_limited_until = ?1 WHERE id = ?2",
                                rusqlite::params![until, account.id],
                            );
                        }
                    }
                    return AccountResult::RateLimited(rate_info);
                }

                // Handle auth failure (401/403)
                if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                    return AccountResult::AuthFailed(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::UNAUTHORIZED));
                }

                // Handle thinking block error (400 + specific error message)
                if status == reqwest::StatusCode::BAD_REQUEST {
                    let resp_body = match upstream_resp.bytes().await {
                        Ok(b) => b,
                        Err(e) => return AccountResult::Error(e.to_string()),
                    };
                    if is_thinking_block_error(StatusCode::BAD_REQUEST, &resp_body) {
                        // Retry with thinking blocks filtered
                        if let Some(filtered) = filter_thinking_blocks(&final_body) {
                            debug!("Retrying with thinking blocks filtered");
                            let retry_resp = client
                                .request(
                                    reqwest::Method::from_bytes(req_meta.method.as_bytes()).unwrap_or(reqwest::Method::POST),
                                    &url,
                                )
                                .headers(reqwest_headers(&upstream_headers))
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
                    // Non-thinking error — return as-is
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

    // Update account stats
    if let Ok(conn) = pool.get() {
        for account in &ordered_accounts {
            let _ = conn.execute(
                "UPDATE accounts SET request_count = request_count + 1, total_requests = total_requests + 1, last_used = ?1 WHERE id = ?2",
                rusqlite::params![now, account.id],
            );
            break; // Only increment for the first (chosen) account
        }
    }

    match result {
        Some(response) => response,
        None => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "All accounts failed — request could not be proxied",
        ),
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
    let is_streaming = provider.is_streaming_response(&axum_headers(&resp_headers));

    // Parse rate limit info and persist
    let rate_info = provider.parse_rate_limit(&axum_headers(&resp_headers), status.as_u16());
    if let Some(pool) = state.db_pool::<DbPool>() {
        if let Ok(conn) = pool.get() {
            let _ = conn.execute(
                "UPDATE accounts SET rate_limit_status = ?1, rate_limit_remaining = ?2, rate_limit_reset = ?3 WHERE id = ?4",
                rusqlite::params![
                    rate_info.status_header,
                    rate_info.remaining,
                    rate_info.reset_time,
                    account.id,
                ],
            );
        }
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
            if let Ok(name) = axum::http::HeaderName::from_bytes(name.as_ref()) {
                if let Ok(value) = axum::http::HeaderValue::from_bytes(value.as_ref()) {
                    builder = builder.header(name, value);
                }
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
            if let Ok(name) = axum::http::HeaderName::from_bytes(name.as_ref()) {
                if let Ok(value) = axum::http::HeaderValue::from_bytes(value.as_ref()) {
                    builder = builder.header(name, value);
                }
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

/// Convert axum HeaderMap to reqwest HeaderMap.
fn reqwest_headers(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_ref()),
            reqwest::header::HeaderValue::from_bytes(value.as_ref()),
        ) {
            out.insert(n, v);
        }
    }
    out
}

/// Convert reqwest HeaderMap to axum HeaderMap.
fn axum_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(n), Ok(v)) = (
            axum::http::HeaderName::from_bytes(name.as_ref()),
            axum::http::HeaderValue::from_bytes(value.as_ref()),
        ) {
            out.insert(n, v);
        }
    }
    out
}
