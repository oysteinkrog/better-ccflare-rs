//! OAuth re-authentication flow — browser-based PKCE auth for expired accounts.
//!
//! Endpoints:
//! - `POST /api/oauth/init/:id` — generates auth URL, stores PKCE verifier
//! - `GET /api/oauth/callback` — exchanges code for tokens (browser redirect)
//! - `POST /api/oauth/complete` — exchanges code for tokens (JSON API)

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use bccf_core::AppState;
use bccf_database::repositories::account as account_repo;
use bccf_database::DbPool;
use bccf_providers::ProviderRegistry;

const PENDING_TTL_MS: i64 = 10 * 60 * 1000; // 10 minutes

// ---------------------------------------------------------------------------
// DB-backed PKCE verifier store (survives restarts)
// ---------------------------------------------------------------------------

/// Store a pending PKCE verifier in the oauth_sessions table.
fn db_insert_pending(
    pool: &DbPool,
    csrf_token: &str,
    account_id: &str,
    verifier: &str,
) -> Result<(), String> {
    let conn = pool.get().map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    let expires_at = now + PENDING_TTL_MS;

    // Cleanup expired entries
    let _ = conn.execute(
        "DELETE FROM oauth_sessions WHERE expires_at < ?1",
        rusqlite::params![now],
    );

    conn.execute(
        "INSERT OR REPLACE INTO oauth_sessions (id, account_name, verifier, mode, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![csrf_token, account_id, verifier, "reauth", now, expires_at],
    ).map_err(|e| e.to_string())?;

    Ok(())
}

/// Take (and delete) a pending PKCE verifier by CSRF token. Returns (account_id, verifier).
///
/// Uses a transaction so that concurrent callbacks with the same CSRF token
/// cannot both read the row before either deletes it (PKCE race condition).
fn db_take_pending(pool: &DbPool, csrf_token: &str) -> Option<(String, String)> {
    let mut conn = pool.get().ok()?;
    let now = chrono::Utc::now().timestamp_millis();

    let tx = conn.transaction().ok()?;

    let result = tx
        .query_row(
            "SELECT account_name, verifier FROM oauth_sessions WHERE id = ?1 AND expires_at > ?2",
            rusqlite::params![csrf_token, now],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()?;

    // Delete within the same transaction — one-time use, atomic with the SELECT
    tx.execute(
        "DELETE FROM oauth_sessions WHERE id = ?1",
        rusqlite::params![csrf_token],
    )
    .ok()?;

    tx.commit().ok()?;

    Some(result)
}

// ---------------------------------------------------------------------------
// Init endpoint
// ---------------------------------------------------------------------------

/// `POST /api/oauth/init/:id` — start OAuth flow for an account.
///
/// Returns JSON `{ "url": "https://claude.ai/login?..." }`.
/// The frontend opens this URL in a new tab.
pub async fn oauth_init(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        warn!("Database pool not available for OAuth init");
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    // Verify account exists and is an OAuth provider
    let account = match pool.get() {
        Ok(conn) => match account_repo::find_by_id(&conn, &account_id) {
            Ok(Some(a)) => a,
            Ok(None) => return error_json(StatusCode::NOT_FOUND, "Account not found"),
            Err(e) => {
                warn!(account_id = %account_id, error = %e, "Failed to look up account");
                return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
            }
        },
        Err(e) => {
            warn!(error = %e, "Failed to get DB connection");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    if account.provider != "anthropic" && account.provider != "claude-oauth" {
        return error_json(
            StatusCode::BAD_REQUEST,
            "Only OAuth accounts can be re-authenticated via browser",
        );
    }

    // Get OAuth provider and config
    let Some(registry) = state.provider_registry::<ProviderRegistry>() else {
        warn!("Provider registry not available for OAuth init");
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };
    let provider_name = if account.provider == "anthropic" {
        "claude-oauth"
    } else {
        &account.provider
    };
    let Some(oauth_provider) = registry.get_oauth(provider_name) else {
        warn!(provider = %provider_name, "OAuth provider not found in registry");
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    let client_id = state.config().get_runtime().client_id.clone();

    // Generate CSRF state
    let csrf = bccf_providers::impls::claude_oauth::CsrfState::generate();
    let csrf_encoded = match csrf.encode() {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to encode CSRF state");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    // Generate auth URL + PKCE
    let (auth_url, verifier) = match oauth_provider
        .generate_auth_url(&csrf_encoded, &client_id)
        .await
    {
        Ok(pair) => pair,
        Err(e) => {
            warn!(error = %e, "Failed to generate OAuth auth URL");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    // Store PKCE verifier in database (survives server restarts)
    if let Err(e) = db_insert_pending(pool, &csrf.csrf_token, &account_id, &verifier) {
        warn!(error = %e, "Failed to store PKCE state");
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    }

    (StatusCode::OK, Json(json!({ "url": auth_url }))).into_response()
}

// ---------------------------------------------------------------------------
// Callback endpoint (browser redirect — returns HTML)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

/// `GET /api/oauth/callback` — handle OAuth redirect from Anthropic.
pub async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let Some(code) = params.code.as_deref() else {
        return render_callback_page(false, "Invalid callback request");
    };

    match exchange_and_persist(&state, code, params.state.as_deref()).await {
        Ok(msg) => render_callback_page(true, &msg),
        Err(msg) => render_callback_page(false, &msg),
    }
}

// ---------------------------------------------------------------------------
// Complete endpoint (JSON API — called from dashboard JS)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CompleteParams {
    pub code: String,
}

/// `POST /api/oauth/complete` — exchange code for tokens (JSON response).
///
/// The dashboard calls this after the user pastes the code/URL from the
/// Anthropic callback page.
pub async fn oauth_complete(
    State(state): State<Arc<AppState>>,
    Json(params): Json<CompleteParams>,
) -> Response {
    let (code, explicit_state) = extract_code_from_input(&params.code);

    match exchange_and_persist(&state, &code, explicit_state.as_deref()).await {
        Ok(msg) => (
            StatusCode::OK,
            Json(json!({ "success": true, "message": msg })),
        )
            .into_response(),
        Err(msg) => (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response(),
    }
}

/// Extract code and state from user input.
///
/// Returns `(code, Option<state>)`. Handles:
/// - Full URL with query params: `https://...?code=ABC&state=XYZ`
/// - Full URL with fragment: `https://...?code=ABC#STATE`
/// - Code#state format: `ABC#STATE`
/// - Just code: `ABC`
fn extract_code_from_input(input: &str) -> (String, Option<String>) {
    let trimmed = input.trim();

    // If it looks like a URL, extract query parameters
    if trimmed.starts_with("http") {
        let mut code: Option<String> = None;
        let mut state: Option<String> = None;

        // Split off fragment (#...) first
        let (url_part, fragment) = if let Some(hash_pos) = trimmed.find('#') {
            (&trimmed[..hash_pos], Some(&trimmed[hash_pos + 1..]))
        } else {
            (trimmed, None)
        };

        // Extract query parameters
        if let Some(query_pos) = url_part.find('?') {
            let query = &url_part[query_pos + 1..];
            for param in query.split('&') {
                if let Some((key, value)) = param.split_once('=') {
                    match key {
                        "code" => code = Some(value.replace("%23", "#")),
                        "state" => state = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }

        if let Some(c) = code {
            if state.is_none() {
                if let Some(frag) = fragment {
                    if !frag.is_empty() {
                        state = Some(frag.to_string());
                    }
                }
            }
            return (c, state);
        }
    }

    // Not a URL — check for code#state format
    if trimmed.contains('#') {
        let parts: Vec<&str> = trimmed.splitn(2, '#').collect();
        return (parts[0].to_string(), Some(parts[1].to_string()));
    }

    (trimmed.to_string(), None)
}

// ---------------------------------------------------------------------------
// Shared token exchange logic
// ---------------------------------------------------------------------------

/// Exchange an authorization code for tokens and persist them.
async fn exchange_and_persist(
    state: &AppState,
    code_input: &str,
    explicit_state: Option<&str>,
) -> Result<String, String> {
    // Extract CSRF state from code (Anthropic embeds it as "code#state")
    let (code, csrf_encoded) = if let Some(s) = explicit_state {
        (code_input.to_string(), s.to_string())
    } else if code_input.contains('#') {
        let parts: Vec<&str> = code_input.splitn(2, '#').collect();
        (parts[0].to_string(), parts[1].to_string())
    } else {
        return Err("Missing OAuth state. Paste the full URL or the code#state value from the callback page.".to_string());
    };

    // Decode and validate CSRF state
    let csrf = bccf_providers::impls::claude_oauth::CsrfState::decode(&csrf_encoded)
        .map_err(|e| {
            warn!(error = %e, "Failed to decode CSRF state");
            "Invalid OAuth state parameter".to_string()
        })?;

    // Look up pending auth by CSRF token (from database)
    let pool = state.db_pool::<DbPool>().ok_or_else(|| {
        warn!("Database pool not available during OAuth exchange");
        "Internal server error".to_string()
    })?;
    let (account_id, verifier) = db_take_pending(pool, &csrf.csrf_token)
        .ok_or("OAuth session expired or already used. Click Re-auth to start a new flow.")?;

    // Get provider and exchange code
    let registry = state
        .provider_registry::<ProviderRegistry>()
        .ok_or_else(|| {
            warn!("Provider registry not available during OAuth exchange");
            "Internal server error".to_string()
        })?;
    let oauth_provider = registry.get_oauth("claude-oauth").ok_or_else(|| {
        warn!("OAuth provider not found in registry");
        "Internal server error".to_string()
    })?;

    let client_id = state.config().get_runtime().client_id.clone();

    let tokens = oauth_provider
        .exchange_code(&code, &csrf_encoded, &verifier, &client_id)
        .await
        .map_err(|e| {
            warn!(account_id = %account_id, error = %e, "OAuth code exchange failed");
            "Authentication failed".to_string()
        })?;

    // Persist tokens to database
    let conn = pool.get().map_err(|e| {
        warn!(error = %e, "Failed to get DB connection for token persistence");
        "Internal server error".to_string()
    })?;
    account_repo::update_tokens(
        &conn,
        &account_id,
        &tokens.access_token,
        tokens.expires_at,
        Some(tokens.refresh_token.as_str()),
    )
    .map_err(|e| {
        warn!(account_id = %account_id, error = %e, "Failed to persist OAuth tokens");
        "Internal server error".to_string()
    })?;

    // Successful re-auth should immediately clear any stale auth/rate-limit markers
    // so dashboard/API status reflects the new healthy token.
    let _ = conn.execute(
        "UPDATE accounts
         SET rate_limit_status = NULL,
             rate_limit_reset = NULL,
             rate_limit_remaining = NULL,
             rate_limited_until = NULL
         WHERE id = ?1",
        rusqlite::params![account_id],
    );

    // Persist subscription tier if included in token response
    if let Some(ref tier) = tokens.subscription_tier {
        let _ = account_repo::set_subscription_tier(&conn, &account_id, Some(tier));
    }

    // Persist email if included in token response
    if let Some(ref email) = tokens.email {
        let _ = account_repo::set_email(&conn, &account_id, Some(email));
    }

    // Clear any token manager backoff state
    if let Some(tm) = state.token_manager::<crate::token_manager::TokenManager>() {
        tm.clear_account_state(&account_id);
    }

    info!(account_id = %account_id, "OAuth re-authentication successful");
    Ok("Account re-authenticated successfully!".to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn error_json(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_callback_page(success: bool, message: &str) -> Response {
    let (icon, color, title) = if success {
        ("&#10003;", "#22c55e", "Re-authenticated")
    } else {
        ("&#10007;", "#ef4444", "Authentication Failed")
    };

    let message = html_escape(message);

    let html = format!(
        r#"<!doctype html>
<html lang="en" data-theme="dark">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>better-ccflare — OAuth</title>
  <style>
    body {{ background: #1a1a2e; color: #e0e0e0; font-family: system-ui, sans-serif;
           display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; }}
    .card {{ text-align: center; padding: 2rem 3rem; border-radius: 12px;
             background: #16213e; box-shadow: 0 4px 20px rgba(0,0,0,0.3); max-width: 400px; }}
    .icon {{ font-size: 3rem; color: {color}; margin-bottom: 0.5rem; }}
    h2 {{ margin: 0.5rem 0; color: {color}; }}
    p {{ color: #a0a0b0; margin: 0.5rem 0 1.5rem; }}
    .close-msg {{ font-size: 0.85rem; color: #606080; }}
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">{icon}</div>
    <h2>{title}</h2>
    <p>{msg}</p>
    <p class="close-msg">This tab will close automatically...</p>
  </div>
  <script>
    if (window.opener) {{
      try {{ window.opener.postMessage('oauth-complete', '*'); }} catch(e) {{}}
    }}
    setTimeout(function() {{ window.close(); }}, 2000);
  </script>
</body>
</html>"#,
        color = color,
        icon = icon,
        title = title,
        msg = message,
    );

    Html(html).into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_json_does_not_leak_internal_details() {
        // Simulate the pattern: an internal error (e.g. DB, upstream provider)
        // should result in a generic client message, not the raw error.
        let upstream_error = "connection refused: oauth.anthropic.com:443 TLS handshake timeout";

        // The sanitized message that should go to the client:
        let client_msg = "Authentication failed";

        let response = error_json(StatusCode::BAD_REQUEST, client_msg);
        let (parts, body) = response.into_parts();

        // Collect body synchronously (it's a small JSON blob)
        let body_bytes = axum::body::to_bytes(body, 4096);
        let body_bytes = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_bytes)
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);

        // Client response MUST contain the generic message
        assert!(body_str.contains("Authentication failed"));
        // Client response MUST NOT contain upstream error details
        assert!(
            !body_str.contains(upstream_error),
            "Response must not contain upstream error: {body_str}"
        );
        assert!(
            !body_str.contains("oauth.anthropic.com"),
            "Response must not contain upstream hostnames: {body_str}"
        );
        assert!(
            !body_str.contains("TLS handshake"),
            "Response must not contain connection details: {body_str}"
        );
        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn callback_page_does_not_leak_upstream_error() {
        let upstream_error =
            "invalid_grant: refresh token revoked by provider at /internal/oauth2/revoke";

        // After sanitization, client sees only a generic message
        let sanitized = "Authentication failed";
        let response = render_callback_page(false, sanitized);
        let (_, body) = response.into_parts();

        let body_bytes = axum::body::to_bytes(body, 8192);
        let body_bytes = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_bytes)
            .unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);

        assert!(body_str.contains("Authentication failed"));
        assert!(
            !body_str.contains(upstream_error),
            "Callback page must not contain upstream error: {body_str}"
        );
        assert!(
            !body_str.contains("invalid_grant"),
            "Callback page must not contain OAuth error codes: {body_str}"
        );
        assert!(
            !body_str.contains("/internal/"),
            "Callback page must not contain internal paths: {body_str}"
        );
    }

    #[test]
    fn extract_code_from_url() {
        let (code, state) =
            extract_code_from_input("https://example.com/callback?code=ABC123&state=XYZ");
        assert_eq!(code, "ABC123");
        assert_eq!(state, Some("XYZ".to_string()));
    }

    #[test]
    fn extract_code_from_hash_format() {
        let (code, state) = extract_code_from_input("ABC123#STATE456");
        assert_eq!(code, "ABC123");
        assert_eq!(state, Some("STATE456".to_string()));
    }

    #[test]
    fn extract_code_plain() {
        let (code, state) = extract_code_from_input("ABC123");
        assert_eq!(code, "ABC123");
        assert_eq!(state, None);
    }
}
