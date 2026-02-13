//! Dashboard route handlers and router construction.
//!
//! Routes:
//! - `GET /dashboard` — redirect to overview
//! - `GET /dashboard/{tab}` — full page or HTMX fragment
//! - `GET /dashboard/assets/{file}` — embedded static assets

use std::sync::Arc;

use askama::Template;
use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;

use bccf_core::AppState;

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
async fn dashboard_tab(Path(tab): Path<String>, headers: HeaderMap) -> Response {
    let is_htmx = headers.contains_key("hx-request");
    let version = bccf_core::get_version();

    // Render the tab fragment
    let tab_html = match tab.as_str() {
        "overview" => OverviewTab.render(),
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
