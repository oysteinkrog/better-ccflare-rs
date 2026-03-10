//! API key authentication middleware.
//!
//! Verifies requests against stored API key hashes using scrypt (with SHA-256 legacy fallback).
//! Supports path-based exemptions for health, OAuth, and dashboard routes.
//!
//! Performance: scrypt verification is offloaded to `spawn_blocking` to avoid blocking
//! the async runtime. Verified keys are cached for 5 minutes to avoid repeated scrypt
//! computations on every request.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tracing::debug;

use bccf_core::AppState;
use bccf_database::DbPool;

/// Authentication result passed through request extensions.
#[derive(Debug, Clone, Default)]
pub struct AuthInfo {
    /// Whether the request was authenticated.
    pub is_authenticated: bool,
    /// ID of the API key used (if any).
    pub api_key_id: Option<String>,
    /// Name of the API key used (if any).
    pub api_key_name: Option<String>,
}

// ---------------------------------------------------------------------------
// API key cache
// ---------------------------------------------------------------------------

/// TTL for cached API key verifications (successful).
const API_KEY_CACHE_TTL_SECS: u64 = 300; // 5 minutes

/// TTL for cached negative (failed) API key verifications.
const API_KEY_NEGATIVE_CACHE_TTL_SECS: u64 = 30; // 30 seconds

/// TTL for cached "auth enabled" check.
const AUTH_ENABLED_CACHE_TTL_SECS: u64 = 10;

/// Maximum number of entries in the API key cache (cleared on overflow).
const API_KEY_CACHE_MAX_SIZE: usize = 1000;

struct CachedKeyResult {
    /// Some((id, name)) for successful verifications; None for failed ones.
    result: Option<(String, String)>,
    verified_at: Instant,
}

struct AuthEnabledCache {
    enabled: bool,
    checked_at: Instant,
}

fn api_key_cache() -> &'static RwLock<HashMap<String, CachedKeyResult>> {
    static CACHE: OnceLock<RwLock<HashMap<String, CachedKeyResult>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Sentinel error type to distinguish DB errors from "0 keys" in auth checks.
#[derive(Debug)]
enum AuthCountError {
    DbError,
}

/// Count active API keys; returns Err on connection/query failure.
fn count_active_api_keys_sync(pool: &DbPool) -> Result<i64, AuthCountError> {
    let conn = pool.get().map_err(|_| AuthCountError::DbError)?;
    conn.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE is_active = 1",
        [],
        |row| row.get(0),
    )
    .map_err(|_| AuthCountError::DbError)
}

fn auth_enabled_cache() -> &'static RwLock<Option<AuthEnabledCache>> {
    static CACHE: OnceLock<RwLock<Option<AuthEnabledCache>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

// ---------------------------------------------------------------------------
// Key extraction
// ---------------------------------------------------------------------------

/// Extract the API key from the request headers.
///
/// Priority:
/// 1. `x-api-key` header (Anthropic format)
/// 2. `Authorization: Bearer <key>` header
fn extract_api_key(req: &Request<Body>) -> Option<String> {
    // 1. x-api-key header
    if let Some(val) = req.headers().get("x-api-key") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // 2. Authorization: Bearer <key>
    if let Some(val) = req.headers().get("authorization") {
        if let Ok(s) = val.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                let trimmed = token.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Dashboard password
// ---------------------------------------------------------------------------

/// Get the `DASHBOARD_PASSWORD` env var if set and non-empty.
fn get_dashboard_password() -> Option<String> {
    std::env::var("DASHBOARD_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Extract password from HTTP Basic auth header.
///
/// Decodes `Authorization: Basic base64(user:password)` and returns the password.
/// The username is ignored — only the password is checked against `DASHBOARD_PASSWORD`.
fn extract_basic_auth_password(req: &Request<Body>) -> Option<String> {
    use base64::Engine;

    let val = req.headers().get("authorization")?;
    let s = val.to_str().ok()?;
    let encoded = s.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;
    // Format: "username:password" — we only care about the password
    let password = decoded_str.splitn(2, ':').nth(1)?;
    if password.is_empty() {
        return None;
    }
    Some(password.to_string())
}

/// Constant-time password comparison to prevent timing attacks.
fn verify_password_constant_time(provided: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Build a 401 response. For dashboard/browser paths, includes
/// `WWW-Authenticate: Basic` so the browser shows a native login dialog.
fn auth_failed_response(path: &str, has_dashboard_password: bool) -> Response {
    let normalized = normalize_path(path);
    let is_browser_path = !normalized.starts_with("/api/")
        && !normalized.starts_with("/v1/")
        && !normalized.starts_with("/messages/");

    let mut resp = crate::handler::error_response(
        StatusCode::UNAUTHORIZED,
        "API key required. Include it in the 'x-api-key' header or Authorization: Bearer <key>",
    );

    // Add WWW-Authenticate: Basic for dashboard/browser paths when a dashboard
    // password is configured, so the browser shows a login dialog.
    if is_browser_path && has_dashboard_password {
        resp.headers_mut().insert(
            axum::http::header::WWW_AUTHENTICATE,
            "Basic realm=\"dashboard\"".parse().unwrap(),
        );
    }

    resp
}

// ---------------------------------------------------------------------------
// Path exemptions
// ---------------------------------------------------------------------------

/// Normalize a URL path to prevent bypass via double-slash or similar tricks.
///
/// Collapses repeated leading slashes, e.g. `//api/config` → `/api/config`.
fn normalize_path(path: &str) -> String {
    // Strip all leading slashes, then add exactly one back.
    let stripped = path.trim_start_matches('/');
    format!("/{stripped}")
}

/// Check if a path is exempt from authentication.
///
/// Exempt paths:
/// - `/health` — always exempt (monitoring probes)
/// - `/api/oauth/*` — exempt (needed for OAuth account setup)
/// - `/api/accounts/:id/reload` — exempt (server-to-server reauth)
/// - `/dashboard/assets/*` — exempt (CSS/JS/favicon needed before login)
///
/// All other paths require authentication, including:
/// - `/api/*` — admin/management API
/// - `/v1/*` — proxy endpoints
/// - `/dashboard` and `/dashboard/{tab}` — dashboard UI
/// - `/dashboard/partials/*` — HTMX partials
/// - `/metrics` — Prometheus metrics
/// - `/` — root redirect
fn is_path_exempt(path: &str, _method: &str) -> bool {
    // M1: normalize path before checks to prevent `//api/config` bypass.
    let path = normalize_path(path);
    let path = path.as_str();

    // Health check
    if path == "/health" {
        return true;
    }

    // OAuth endpoints
    if path.starts_with("/api/oauth") {
        return true;
    }

    // Account reload (server-to-server reauth notifications)
    if path.ends_with("/reload") && path.starts_with("/api/accounts/") {
        return true;
    }

    // Dashboard static assets (CSS, JS, favicon) — needed to render login page
    if path.starts_with("/dashboard/assets/") {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Verification (sync — runs inside spawn_blocking)
// ---------------------------------------------------------------------------

/// Verify an API key against stored hashed keys in the database.
///
/// Supports both scrypt hashes (salt:hash format) and legacy SHA-256 hashes.
/// Uses constant-time comparison to prevent timing attacks.
fn verify_api_key_sync(pool: &DbPool, api_key: &str) -> Option<(String, String)> {
    let conn = pool.get().ok()?;

    // Get all active API keys
    let mut stmt = conn
        .prepare("SELECT id, name, hashed_key FROM api_keys WHERE is_active = 1")
        .ok()?;

    let keys: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    for (id, name, stored_hash) in &keys {
        match crate::crypto::verify_api_key(api_key, stored_hash) {
            Ok(true) => return Some((id.clone(), name.clone())),
            _ => continue,
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Async wrappers with caching
// ---------------------------------------------------------------------------

/// Check if auth is enabled (cached for 10 seconds).
///
/// Returns `Ok(true)` if any active API keys exist, `Ok(false)` if none,
/// and `Err(())` if the database could not be queried (fail-closed: treat as enabled).
async fn is_auth_enabled(pool: &DbPool) -> Result<bool, ()> {
    // Check cache (read lock — concurrent readers allowed)
    {
        let cache = auth_enabled_cache().read();
        if let Some(ref entry) = *cache {
            if entry.checked_at.elapsed().as_secs() < AUTH_ENABLED_CACHE_TTL_SECS {
                return Ok(entry.enabled);
            }
        }
    }

    // Cache miss — query via spawn_blocking
    let pool = pool.clone();
    let count_result = tokio::task::spawn_blocking(move || count_active_api_keys_sync(&pool))
        .await
        .unwrap_or(Err(AuthCountError::DbError));

    let enabled = match count_result {
        Ok(count) => count > 0,
        Err(AuthCountError::DbError) => {
            // Fail-closed: treat DB error as "auth is enabled" to avoid bypass.
            // Do NOT cache this result so we retry on the next request.
            return Err(());
        }
    };

    // Update cache (write lock) — only cache definitive results
    {
        let mut cache = auth_enabled_cache().write();
        *cache = Some(AuthEnabledCache {
            enabled,
            checked_at: Instant::now(),
        });
    }

    Ok(enabled)
}

/// Hash an API key for use as a cache key (avoids storing plaintext keys in memory).
/// Uses manual hex encoding to avoid format! allocation overhead.
fn hash_for_cache(api_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    let hash = hasher.finalize();
    // Manual hex encode — 64 chars for SHA-256, avoids format! overhead
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in hash {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Verify an API key with caching and spawn_blocking.
///
/// On cache hit (within TTL), returns immediately without any crypto or DB work.
/// On cache miss, offloads the scrypt verification to a blocking thread.
/// Cache keys are SHA-256 hashed to avoid storing plaintext API keys in memory.
///
/// Both successful and failed verifications are cached to prevent O(N) scrypt DoS
/// from repeated invalid keys. Successful results are cached for 5 minutes;
/// failed results are cached for 30 seconds.
async fn verify_api_key_cached(pool: &DbPool, api_key: &str) -> Option<(String, String)> {
    let cache_key = hash_for_cache(api_key);

    // Check cache (read lock — concurrent readers allowed)
    {
        let cache = api_key_cache().read();
        if let Some(entry) = cache.get(&cache_key) {
            let ttl = if entry.result.is_some() {
                API_KEY_CACHE_TTL_SECS
            } else {
                API_KEY_NEGATIVE_CACHE_TTL_SECS
            };
            if entry.verified_at.elapsed().as_secs() < ttl {
                return entry.result.clone();
            }
        }
    }

    // Cache miss — verify via spawn_blocking (scrypt is CPU-intensive)
    let pool = pool.clone();
    let key_owned = api_key.to_string();
    let result = tokio::task::spawn_blocking(move || verify_api_key_sync(&pool, &key_owned))
        .await
        .ok()
        .flatten();

    // Cache both successful and failed results (write lock).
    // M2 fix: when cache is full, clear it entirely rather than doing an O(N)
    // linear scan under the write lock. At 1000 entries this is acceptable.
    {
        let mut cache = api_key_cache().write();
        if cache.len() >= API_KEY_CACHE_MAX_SIZE {
            cache.clear();
        }
        cache.insert(
            cache_key,
            CachedKeyResult {
                result: result.clone(),
                verified_at: Instant::now(),
            },
        );
    }

    result
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Authentication middleware for axum.
///
/// Checks path exemptions, then tries (in order):
/// 1. `DASHBOARD_PASSWORD` match (cheap constant-time comparison)
/// 2. API key verification against DB (expensive scrypt)
///
/// If no API keys exist AND no `DASHBOARD_PASSWORD` is set, all requests
/// are allowed (first-run experience).
///
/// For dashboard/browser paths, a 401 includes `WWW-Authenticate: Basic`
/// so the browser shows a native login dialog.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().as_str().to_string();

    // Check if path is exempt
    if is_path_exempt(&path, &method) {
        req.extensions_mut().insert(AuthInfo {
            is_authenticated: true,
            ..Default::default()
        });
        return next.run(req).await;
    }

    // Check what auth mechanisms are available
    let dashboard_password = get_dashboard_password();
    let has_dashboard_password = dashboard_password.is_some();

    let pool = state.db_pool::<DbPool>();
    let auth_enabled = match pool {
        Some(p) => match is_auth_enabled(p).await {
            Ok(enabled) => enabled,
            Err(()) => {
                // DB error — fail closed unless dashboard password provides a fallback
                if !has_dashboard_password {
                    return crate::handler::error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Authentication check failed due to a database error",
                    );
                }
                false
            }
        },
        None => false,
    };

    // First-run: no API keys AND no dashboard password.
    // Only allow GET /dashboard (and sub-paths) through unauthenticated.
    // All other paths (/api/*, /v1/*, /metrics, etc.) require auth even on first run.
    if !auth_enabled && !has_dashboard_password {
        let normalized = normalize_path(&path);
        let is_dashboard_get = method == "GET"
            && (normalized == "/dashboard"
                || normalized.starts_with("/dashboard/"));
        if is_dashboard_get {
            req.extensions_mut().insert(AuthInfo {
                is_authenticated: true,
                ..Default::default()
            });
            return next.run(req).await;
        }
        // Not a dashboard GET — return 401 even in first-run mode.
        return auth_failed_response(&path, false);
    }

    // CSRF protection: POST/PUT/DELETE/PATCH requests to /api/* must include
    // either X-Requested-With or Content-Type: application/json.
    // API key bearer requests are exempt (bearer tokens are not sent automatically
    // by browsers, so CSRF only applies to cookie/session-style auth).
    {
        let normalized = normalize_path(&path);
        let is_mutating_method = matches!(
            method.as_str(),
            "POST" | "PUT" | "DELETE" | "PATCH"
        );
        let is_api_path = normalized.starts_with("/api/");
        let has_bearer = extract_api_key(&req).is_some();

        if is_mutating_method && is_api_path && !has_bearer {
            let has_x_requested_with = req
                .headers()
                .get("x-requested-with")
                .is_some();
            let has_json_content_type = req
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.starts_with("application/json"))
                .unwrap_or(false);

            if !has_x_requested_with && !has_json_content_type {
                return crate::handler::error_response(
                    StatusCode::FORBIDDEN,
                    "CSRF check failed: missing X-Requested-With or application/json content type",
                );
            }
        }
    }

    // Extract credentials
    let api_key = extract_api_key(&req);
    let basic_password = extract_basic_auth_password(&req);

    // 1. Check DASHBOARD_PASSWORD (cheap — constant-time string comparison)
    if let Some(ref expected) = dashboard_password {
        // Check Basic auth password
        if let Some(ref pwd) = basic_password {
            if verify_password_constant_time(pwd, expected) {
                debug!("Auth success: dashboard password (Basic) for {path}");
                req.extensions_mut().insert(AuthInfo {
                    is_authenticated: true,
                    ..Default::default()
                });
                return next.run(req).await;
            }
        }
        // Check API key / Bearer token against dashboard password
        if let Some(ref key) = api_key {
            if verify_password_constant_time(key, expected) {
                debug!("Auth success: dashboard password (Bearer) for {path}");
                req.extensions_mut().insert(AuthInfo {
                    is_authenticated: true,
                    ..Default::default()
                });
                return next.run(req).await;
            }
        }
    }

    // 2. Check API key against DB (expensive — scrypt, only if keys exist)
    if auth_enabled {
        if let Some(ref key) = api_key {
            if let Some(pool) = pool {
                if let Some((id, name)) = verify_api_key_cached(pool, key).await {
                    debug!("Auth success: key={name} path={path}");
                    req.extensions_mut().insert(AuthInfo {
                        is_authenticated: true,
                        api_key_id: Some(id),
                        api_key_name: Some(name),
                    });
                    return next.run(req).await;
                }
            }
        }
    }

    // 3. Auth failed
    debug!("Auth failed: no valid credential for {path}");
    auth_failed_response(&path, has_dashboard_password)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Path exemption tests --

    #[test]
    fn health_is_exempt() {
        assert!(is_path_exempt("/health", "GET"));
    }

    #[test]
    fn oauth_is_exempt() {
        assert!(is_path_exempt("/api/oauth/init", "POST"));
        assert!(is_path_exempt("/api/oauth/callback", "POST"));
    }

    #[test]
    fn reload_is_exempt() {
        assert!(is_path_exempt("/api/accounts/abc123/reload", "POST"));
    }

    #[test]
    fn dashboard_assets_are_exempt() {
        assert!(is_path_exempt("/dashboard/assets/pico.min.css", "GET"));
        assert!(is_path_exempt("/dashboard/assets/htmx.min.js", "GET"));
        assert!(is_path_exempt("/dashboard/assets/favicon.svg", "GET"));
    }

    #[test]
    fn dashboard_pages_require_auth() {
        assert!(!is_path_exempt("/dashboard", "GET"));
        assert!(!is_path_exempt("/dashboard/overview", "GET"));
        assert!(!is_path_exempt("/dashboard/accounts", "GET"));
        assert!(!is_path_exempt("/dashboard/partials/overview", "GET"));
    }

    #[test]
    fn metrics_requires_auth() {
        assert!(!is_path_exempt("/metrics", "GET"));
    }

    #[test]
    fn root_requires_auth() {
        assert!(!is_path_exempt("/", "GET"));
    }

    #[test]
    fn api_requires_auth() {
        assert!(!is_path_exempt("/api/stats", "GET"));
        assert!(!is_path_exempt("/api/accounts", "GET"));
        assert!(!is_path_exempt("/api/config", "GET"));
    }

    #[test]
    fn double_slash_path_not_exempt() {
        // M1: double-slash bypass must not allow protected paths to be exempt
        assert!(!is_path_exempt("//api/config", "GET"));
        assert!(!is_path_exempt("//api/stats", "GET"));
        assert!(!is_path_exempt("//v1/messages", "POST"));
        assert!(!is_path_exempt("//dashboard", "GET"));
        assert!(!is_path_exempt("//metrics", "GET"));
    }

    #[test]
    fn double_slash_exempt_paths_still_work() {
        // Normalized exempt paths should still be exempt
        assert!(is_path_exempt("//health", "GET"));
        assert!(is_path_exempt("//api/oauth/init", "POST"));
        assert!(is_path_exempt("//dashboard/assets/pico.min.css", "GET"));
    }

    #[test]
    fn proxy_requires_auth() {
        assert!(!is_path_exempt("/v1/messages", "POST"));
        assert!(!is_path_exempt("/v1/models", "GET"));
    }

    // -- First-run bypass narrowing tests --

    /// Helper: returns true if the request would pass the first-run bypass check
    /// (GET /dashboard or /dashboard/* only).
    fn first_run_bypass_allowed(path: &str, method: &str) -> bool {
        let normalized = normalize_path(path);
        method == "GET"
            && (normalized == "/dashboard" || normalized.starts_with("/dashboard/"))
    }

    #[test]
    fn first_run_allows_get_dashboard() {
        assert!(first_run_bypass_allowed("/dashboard", "GET"));
        assert!(first_run_bypass_allowed("/dashboard/overview", "GET"));
        assert!(first_run_bypass_allowed("/dashboard/accounts", "GET"));
    }

    #[test]
    fn first_run_rejects_post_api_accounts() {
        assert!(!first_run_bypass_allowed("/api/accounts", "POST"));
    }

    #[test]
    fn first_run_rejects_get_api_stats() {
        assert!(!first_run_bypass_allowed("/api/stats", "GET"));
    }

    // -- CSRF check tests --

    /// Returns true if the CSRF check passes (i.e. the request is allowed through).
    fn csrf_check_passes(path: &str, method: &str, headers: &[(&str, &str)]) -> bool {
        let normalized = normalize_path(path);
        let is_mutating = matches!(method, "POST" | "PUT" | "DELETE" | "PATCH");
        let is_api = normalized.starts_with("/api/");

        // Build a simple request to test header extraction
        let mut builder = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let req = builder.body(Body::empty()).unwrap();

        let has_bearer = extract_api_key(&req).is_some();

        if !is_mutating || !is_api || has_bearer {
            // CSRF check doesn't apply
            return true;
        }

        let has_x_requested_with = req.headers().get("x-requested-with").is_some();
        let has_json_content_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.starts_with("application/json"))
            .unwrap_or(false);

        has_x_requested_with || has_json_content_type
    }

    #[test]
    fn csrf_post_api_without_headers_fails() {
        assert!(!csrf_check_passes("/api/accounts/123/pause", "POST", &[]));
    }

    #[test]
    fn csrf_post_api_with_x_requested_with_passes() {
        assert!(csrf_check_passes(
            "/api/accounts/123/pause",
            "POST",
            &[("x-requested-with", "XMLHttpRequest")]
        ));
    }

    #[test]
    fn csrf_post_api_with_json_content_type_passes() {
        assert!(csrf_check_passes(
            "/api/accounts/123/pause",
            "POST",
            &[("content-type", "application/json")]
        ));
    }

    #[test]
    fn csrf_api_key_bearer_exempt_from_csrf() {
        // Bearer token requests bypass CSRF check
        assert!(csrf_check_passes(
            "/api/accounts/123/pause",
            "POST",
            &[("authorization", "Bearer sk-test-key")]
        ));
    }

    #[test]
    fn csrf_x_api_key_header_exempt_from_csrf() {
        // x-api-key requests bypass CSRF check
        assert!(csrf_check_passes(
            "/api/accounts/123/pause",
            "POST",
            &[("x-api-key", "sk-test-key")]
        ));
    }

    #[test]
    fn csrf_get_requests_not_checked() {
        // GET is not a mutating method — CSRF check does not apply
        assert!(csrf_check_passes("/api/stats", "GET", &[]));
    }

    #[test]
    fn csrf_v1_proxy_not_checked() {
        // /v1/* paths are excluded from CSRF check (different threat model)
        assert!(csrf_check_passes("/v1/messages", "POST", &[]));
    }

    // -- API key extraction tests --

    #[test]
    fn extract_api_key_from_x_api_key() {
        let req = Request::builder()
            .header("x-api-key", "sk-test-123")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_api_key(&req), Some("sk-test-123".to_string()));
    }

    #[test]
    fn extract_api_key_from_bearer() {
        let req = Request::builder()
            .header("authorization", "Bearer sk-test-456")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_api_key(&req), Some("sk-test-456".to_string()));
    }

    #[test]
    fn extract_api_key_prefers_x_api_key() {
        let req = Request::builder()
            .header("x-api-key", "key-from-header")
            .header("authorization", "Bearer key-from-bearer")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_api_key(&req), Some("key-from-header".to_string()));
    }

    #[test]
    fn extract_api_key_none() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(extract_api_key(&req), None);
    }

    #[test]
    fn extract_api_key_empty_ignored() {
        let req = Request::builder()
            .header("x-api-key", "  ")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_api_key(&req), None);
    }

    // -- Basic auth extraction tests --

    #[test]
    fn extract_basic_auth_valid() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:secret123");
        let req = Request::builder()
            .header("authorization", format!("Basic {encoded}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            extract_basic_auth_password(&req),
            Some("secret123".to_string())
        );
    }

    #[test]
    fn extract_basic_auth_empty_password() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("admin:");
        let req = Request::builder()
            .header("authorization", format!("Basic {encoded}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_auth_password(&req), None);
    }

    #[test]
    fn extract_basic_auth_no_colon() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("nocolon");
        let req = Request::builder()
            .header("authorization", format!("Basic {encoded}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_auth_password(&req), None);
    }

    #[test]
    fn extract_basic_auth_bearer_not_matched() {
        let req = Request::builder()
            .header("authorization", "Bearer some-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_basic_auth_password(&req), None);
    }

    // -- Password verification tests --

    #[test]
    fn verify_password_matching() {
        assert!(verify_password_constant_time("secret", "secret"));
    }

    #[test]
    fn verify_password_not_matching() {
        assert!(!verify_password_constant_time("secret", "wrong"));
    }

    #[test]
    fn verify_password_different_lengths() {
        assert!(!verify_password_constant_time("short", "muchlongerpassword"));
    }
}
