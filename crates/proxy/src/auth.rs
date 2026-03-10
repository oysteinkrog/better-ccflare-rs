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
/// - `/health` — always exempt
/// - `/api/oauth/*` — exempt (needed for account setup)
/// - Non-API paths — exempt (dashboard static assets)
/// - `/api/accounts/:id/reload` — exempt (server-to-server)
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

    // Non-API, non-proxy paths are exempt (dashboard, static assets)
    if !path.starts_with("/api/") && !path.starts_with("/v1/") && !path.starts_with("/messages/") {
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
/// Checks API key auth, applies path exemptions, and injects `AuthInfo`
/// into request extensions for downstream handlers.
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

    // Check if authentication is enabled (any active API keys exist)
    let pool = state.db_pool::<DbPool>();
    let auth_enabled = match pool {
        Some(p) => match is_auth_enabled(p).await {
            Ok(enabled) => enabled,
            Err(()) => {
                // DB error — fail closed: return 500 rather than bypassing auth.
                return crate::handler::error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Authentication check failed due to a database error",
                );
            }
        },
        None => false,
    };

    if !auth_enabled {
        // No API keys configured — allow all requests (first-run experience)
        req.extensions_mut().insert(AuthInfo {
            is_authenticated: true,
            ..Default::default()
        });
        return next.run(req).await;
    }

    // Extract API key from request
    let api_key = match extract_api_key(&req) {
        Some(key) => key,
        None => {
            debug!("Auth failed: no API key in request to {path}");
            return crate::handler::error_response(
                StatusCode::UNAUTHORIZED,
                "API key required. Include it in the 'x-api-key' header or Authorization: Bearer <key>",
            );
        }
    };

    // Verify API key (cached + spawn_blocking for scrypt)
    let Some(pool) = pool else {
        // No database — can't verify
        req.extensions_mut().insert(AuthInfo {
            is_authenticated: true,
            ..Default::default()
        });
        return next.run(req).await;
    };

    match verify_api_key_cached(pool, &api_key).await {
        Some((id, name)) => {
            debug!("Auth success: key={name} path={path}");
            req.extensions_mut().insert(AuthInfo {
                is_authenticated: true,
                api_key_id: Some(id),
                api_key_name: Some(name),
            });
            next.run(req).await
        }
        None => {
            debug!("Auth failed: invalid API key for {path}");
            crate::handler::error_response(StatusCode::UNAUTHORIZED, "Invalid API key")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dashboard_is_exempt() {
        assert!(is_path_exempt("/", "GET"));
        assert!(is_path_exempt("/dashboard", "GET"));
        assert!(is_path_exempt("/assets/app.js", "GET"));
    }

    #[test]
    fn api_requires_auth() {
        assert!(!is_path_exempt("/api/stats", "GET"));
        assert!(!is_path_exempt("/api/accounts", "GET"));
        assert!(!is_path_exempt("/api/config", "GET"));
    }

    #[test]
    fn double_slash_path_not_exempt() {
        // M1: double-slash bypass must not allow /api/* paths to be exempt
        assert!(!is_path_exempt("//api/config", "GET"));
        assert!(!is_path_exempt("//api/stats", "GET"));
        assert!(!is_path_exempt("//v1/messages", "POST"));
    }

    #[test]
    fn double_slash_exempt_paths_still_work() {
        // Normalized exempt paths should still be exempt
        assert!(is_path_exempt("//health", "GET"));
        assert!(is_path_exempt("//api/oauth/init", "POST"));
        assert!(is_path_exempt("//dashboard", "GET"));
    }

    #[test]
    fn proxy_requires_auth() {
        assert!(!is_path_exempt("/v1/messages", "POST"));
        assert!(!is_path_exempt("/v1/models", "GET"));
    }

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
}
