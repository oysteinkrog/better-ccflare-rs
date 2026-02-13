//! Dashboard route handlers and router construction.
//!
//! Routes:
//! - `GET /dashboard` — redirect to overview
//! - `GET /dashboard/{tab}` — full page or HTMX fragment
//! - `GET /dashboard/partials/overview` — overview stats partial (HTMX refresh)
//! - `GET /dashboard/assets/{file}` — embedded static assets

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;

use bccf_core::AppState;
use bccf_database::DbPool;

use crate::templates::*;

// ---------------------------------------------------------------------------
// Embedded static assets
// ---------------------------------------------------------------------------

const PICO_CSS: &str = include_str!("../assets/pico.min.css");
const HTMX_JS: &str = include_str!("../assets/htmx.min.js");

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the dashboard router. Mount at `/dashboard`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dashboard", get(dashboard_root))
        .route("/dashboard/partials/overview", get(overview_partial))
        .route(
            "/dashboard/partials/accounts-table",
            get(accounts_table_partial),
        )
        .route("/dashboard/{tab}", get(dashboard_tab))
        .route("/dashboard/assets/{file}", get(serve_asset))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /dashboard — redirect to /dashboard/overview.
async fn dashboard_root() -> Redirect {
    Redirect::to("/dashboard/overview")
}

/// GET /dashboard/{tab} — serve a tab.
///
/// If the request has an `HX-Request` header (HTMX), return just the tab
/// fragment. Otherwise, return the full page with the tab content embedded.
async fn dashboard_tab(
    State(state): State<Arc<AppState>>,
    Path(tab): Path<String>,
    headers: HeaderMap,
) -> Response {
    let is_htmx = headers.contains_key("hx-request");
    let version = bccf_core::get_version();

    // Render the tab fragment
    let tab_html = match tab.as_str() {
        "overview" => build_overview(&state).render(),
        "accounts" => AccountsTab.render(),
        "requests" => RequestsTab.render(),
        "analytics" => AnalyticsTab.render(),
        "stats" => StatsTab.render(),
        "logs" => LogsTab.render(),
        "agents" => AgentsTab.render(),
        "api-keys" => ApiKeysTab.render(),
        _ => {
            return (StatusCode::NOT_FOUND, Html("Tab not found".to_string())).into_response();
        }
    };

    let tab_content = match tab_html {
        Ok(html) => html,
        Err(e) => {
            tracing::error!("Template render error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Template error".to_string()),
            )
                .into_response();
        }
    };

    // HTMX request: return just the fragment
    if is_htmx {
        return Html(tab_content).into_response();
    }

    // Full page request: wrap in base layout
    let page = BasePage {
        version,
        tabs: TABS,
        active_tab: &tab,
        tab_content: &tab_content,
    };

    match page.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Base template render error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Template error".to_string()),
            )
                .into_response()
        }
    }
}

/// GET /dashboard/partials/overview — HTMX auto-refresh partial for overview.
async fn overview_partial(State(state): State<Arc<AppState>>) -> Response {
    let tpl = build_overview(&state);
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Overview partial render error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Error loading overview".to_string()),
            )
                .into_response()
        }
    }
}

/// Build an OverviewTab template struct by querying the database.
fn build_overview(state: &AppState) -> OverviewTab {
    let version = bccf_core::get_version().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let Some(pool) = state.db_pool::<DbPool>() else {
        return overview_defaults(version);
    };
    let Ok(conn) = pool.get() else {
        return overview_defaults(version);
    };

    // Aggregated stats
    let aggregated = bccf_database::repositories::stats::get_aggregated_stats(&conn).unwrap_or(
        bccf_database::repositories::stats::AggregatedStats {
            total_requests: 0,
            successful_requests: 0,
            avg_response_time: 0.0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            avg_tokens_per_second: None,
        },
    );

    let success_rate = if aggregated.total_requests > 0 {
        (aggregated.successful_requests as f64 / aggregated.total_requests as f64) * 100.0
    } else {
        0.0
    };

    // Account health
    let accounts = bccf_database::repositories::account::find_all(&conn).unwrap_or_default();
    let total_accounts = accounts.len();
    let active_accounts =
        bccf_database::repositories::stats::get_active_account_count(&conn).unwrap_or(0);
    let paused_accounts = accounts.iter().filter(|a| a.paused).count();
    let rate_limited_accounts = accounts
        .iter()
        .filter(|a| a.rate_limited_until.map(|rl| rl > now).unwrap_or(false))
        .count();
    let healthy_accounts = total_accounts - paused_accounts - rate_limited_accounts;

    // Top models
    let top_models = bccf_database::repositories::stats::get_top_models(&conn, 5)
        .unwrap_or_default()
        .into_iter()
        .map(|m| OverviewModel {
            name: m.model,
            count: m.count,
            percentage: m.percentage,
        })
        .collect();

    // Recent errors
    let recent_errors =
        bccf_database::repositories::stats::get_recent_errors(&conn, 5).unwrap_or_default();

    OverviewTab {
        total_requests: aggregated.total_requests,
        success_rate,
        avg_response_time: aggregated.avg_response_time,
        total_cost_usd: aggregated.total_cost_usd,
        avg_tokens_per_second: aggregated.avg_tokens_per_second.unwrap_or(0.0),
        input_tokens: aggregated.input_tokens,
        output_tokens: aggregated.output_tokens,
        cache_read_tokens: aggregated.cache_read_input_tokens,
        cache_creation_tokens: aggregated.cache_creation_input_tokens,
        total_tokens: aggregated.total_tokens,
        total_accounts,
        active_accounts,
        paused_accounts,
        rate_limited_accounts,
        healthy_accounts,
        recent_errors,
        top_models,
        version,
    }
}

/// Default empty overview when DB is unavailable.
fn overview_defaults(version: String) -> OverviewTab {
    OverviewTab {
        total_requests: 0,
        success_rate: 0.0,
        avg_response_time: 0.0,
        total_cost_usd: 0.0,
        avg_tokens_per_second: 0.0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        total_tokens: 0,
        total_accounts: 0,
        active_accounts: 0,
        paused_accounts: 0,
        rate_limited_accounts: 0,
        healthy_accounts: 0,
        recent_errors: Vec::new(),
        top_models: Vec::new(),
        version,
    }
}

// ---------------------------------------------------------------------------
// Accounts table partial
// ---------------------------------------------------------------------------

/// GET /dashboard/partials/accounts-table — render the accounts table.
async fn accounts_table_partial(State(state): State<Arc<AppState>>) -> Response {
    let now = chrono::Utc::now().timestamp_millis();

    let accounts = match state.db_pool::<DbPool>() {
        Some(pool) => match pool.get() {
            Ok(conn) => bccf_database::repositories::account::find_all(&conn).unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };

    let rows: Vec<AccountRow> = accounts
        .iter()
        .map(|a| {
            let token_status_str = match a.expires_at {
                Some(exp) if exp > now => "valid".to_string(),
                Some(_) => "expired".to_string(),
                None => {
                    if a.api_key.is_some() {
                        "valid".to_string()
                    } else {
                        "expired".to_string()
                    }
                }
            };

            let rate_limit_status = if let Some(until) = a.rate_limited_until {
                if until > now {
                    let minutes_left = ((until - now) as f64 / 60000.0).ceil() as i64;
                    format!("Rate limited ({minutes_left}m)")
                } else {
                    "OK".to_string()
                }
            } else {
                a.rate_limit_status
                    .clone()
                    .unwrap_or_else(|| "OK".to_string())
            };

            let session_info = match a.session_start {
                Some(start) if (now - start) < 5 * 60 * 60 * 1000 => {
                    format!("Active: {} reqs", a.session_request_count)
                }
                _ => "-".to_string(),
            };

            let last_used_relative = a.last_used.map(|ts| {
                let diff_ms = now - ts;
                let secs = diff_ms / 1000;
                if secs < 60 {
                    "just now".to_string()
                } else if secs < 3600 {
                    format!("{}m ago", secs / 60)
                } else if secs < 86400 {
                    format!("{}h ago", secs / 3600)
                } else {
                    format!("{}d ago", secs / 86400)
                }
            });

            AccountRow {
                id: a.id.clone(),
                name: a.name.clone(),
                provider: a.provider.clone(),
                priority: a.priority,
                paused: a.paused,
                token_status_str,
                rate_limit_status,
                session_info,
                request_count: a.request_count,
                total_requests: a.total_requests,
                last_used_relative,
                custom_endpoint: a.custom_endpoint.clone(),
            }
        })
        .collect();

    let tpl = AccountsTablePartial { accounts: rows };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Accounts table render error: {e}");
            Html("<p>Error rendering accounts table</p>".to_string()).into_response()
        }
    }
}

/// GET /dashboard/assets/{file} — serve embedded static assets.
async fn serve_asset(Path(file): Path<String>) -> Response {
    match file.as_str() {
        "pico.min.css" => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/css"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            PICO_CSS,
        )
            .into_response(),
        "htmx.min.js" => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/javascript"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            HTMX_JS,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
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
            "/tmp/bccf-test-dashboard-nonexistent/config.json",
        )))
        .unwrap();
        Arc::new(AppState::new(config))
    }

    #[tokio::test]
    async fn dashboard_root_redirects() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            resp.headers().get("location").unwrap().to_str().unwrap(),
            "/dashboard/overview"
        );
    }

    #[tokio::test]
    async fn overview_full_page() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/overview")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        // Full page should contain the base layout
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("better-ccflare"));
        assert!(html.contains("htmx.min.js"));
        assert!(html.contains("Overview"));
    }

    #[tokio::test]
    async fn overview_htmx_fragment() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/overview")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        // Fragment should NOT contain the base layout
        assert!(!html.contains("<!doctype html>"));
        assert!(html.contains("Overview"));
    }

    #[tokio::test]
    async fn accounts_tab_renders() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/accounts")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Accounts"));
    }

    #[tokio::test]
    async fn unknown_tab_returns_404() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn pico_css_served() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/assets/pico.min.css")
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
            "text/css"
        );

        let body = axum::body::to_bytes(resp.into_body(), 131072)
            .await
            .unwrap();
        assert!(body.len() > 10000); // Pico CSS should be substantial
    }

    #[tokio::test]
    async fn htmx_js_served() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/assets/htmx.min.js")
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
            "application/javascript"
        );

        let body = axum::body::to_bytes(resp.into_body(), 131072)
            .await
            .unwrap();
        assert!(body.len() > 10000); // HTMX should be substantial
    }

    #[tokio::test]
    async fn unknown_asset_returns_404() {
        let state = test_state();
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/dashboard/assets/nonexistent.js")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn all_tabs_render_full_page() {
        let state = test_state();

        for tab in TABS {
            let app = router().with_state(state.clone());
            let req = Request::builder()
                .uri(&format!("/dashboard/{}", tab.slug))
                .body(Body::empty())
                .unwrap();

            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                200,
                "Tab '{}' should render successfully",
                tab.slug
            );
        }
    }
}
