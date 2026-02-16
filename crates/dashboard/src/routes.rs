//! Dashboard route handlers and router construction.
//!
//! Routes:
//! - `GET /dashboard` — redirect to overview
//! - `GET /dashboard/{tab}` — full page or HTMX fragment
//! - `GET /dashboard/partials/overview` — overview stats partial (HTMX refresh)
//! - `GET /dashboard/assets/{file}` — embedded static assets

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use bccf_core::AppState;
use bccf_database::DbPool;

use crate::templates::*;

// ---------------------------------------------------------------------------
// Embedded static assets
// ---------------------------------------------------------------------------

const PICO_CSS: &str = include_str!("../assets/pico.min.css");
const HTMX_JS: &str = include_str!("../assets/htmx.min.js");
const CHART_JS: &str = include_str!("../assets/chart.min.js");
const FAVICON_SVG: &str = include_str!("../assets/favicon.svg");

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
        .route(
            "/dashboard/partials/requests-table",
            get(requests_table_partial),
        )
        .route(
            "/dashboard/partials/agents-table",
            get(agents_table_partial),
        )
        .route(
            "/dashboard/partials/api-keys-table",
            get(api_keys_table_partial),
        )
        .route("/dashboard/partials/stats-table", get(stats_table_partial))
        .route("/dashboard/partials/logs-stream", get(logs_stream_partial))
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

    // Get the usage cache from AppState (populated by UsagePollingService)
    let usage_cache = state.usage_cache::<bccf_providers::UsageCache>();

    // Determine which account the load balancer would choose next.
    // Mirrors the SessionStrategy logic without incrementing round-robin.
    let next_account_id = predict_next_account(&accounts, now);

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

            // Build usage windows from the real provider API data
            let usage_windows = build_usage_windows(usage_cache, &a.id, now);
            let has_usage = !usage_windows.is_empty();

            AccountRow {
                id: a.id.clone(),
                name: a.name.clone(),
                provider: a.provider.clone(),
                priority: a.priority,
                paused: a.paused,
                auto_fallback_enabled: a.auto_fallback_enabled,
                token_status_str,
                rate_limit_status,
                session_info,
                request_count: a.request_count,
                total_requests: a.total_requests,
                last_used_relative,
                custom_endpoint: a.custom_endpoint.clone(),
                usage_windows,
                has_usage,
                is_oauth: a.provider == "anthropic"
                    || a.provider == "claude-oauth"
                    || a.provider == "console",
                is_next: next_account_id.as_deref() == Some(a.id.as_str()),
            }
        })
        .collect();

    let pool_summary = build_pool_summary(&rows);

    let tpl = AccountsTablePartial {
        accounts: rows,
        pool_summary,
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Accounts table render error: {e}");
            Html("<p>Error rendering accounts table</p>".to_string()).into_response()
        }
    }
}

/// Build pool-level aggregate usage summary from all account rows.
fn build_pool_summary(rows: &[AccountRow]) -> Option<PoolUsageSummary> {
    use std::collections::BTreeMap;

    // Only aggregate non-paused accounts that have usage data
    let active_rows: Vec<&AccountRow> = rows
        .iter()
        .filter(|r| !r.paused && r.has_usage && !r.usage_windows.is_empty())
        .collect();

    if active_rows.is_empty() {
        return None;
    }

    // Group windows by label, collecting pct values and reset texts
    let mut groups: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();

    // Maintain insertion order using a separate vec
    let label_order = ["5-hour", "Weekly", "Opus (Wk)", "Sonnet (Wk)", "Daily", "Monthly"];

    for row in &active_rows {
        for w in &row.usage_windows {
            if w.pct >= 0 {
                groups
                    .entry(w.label.clone())
                    .or_default()
                    .push((w.pct, w.reset_text.clone()));
            }
        }
    }

    let mut windows: Vec<PoolWindowSummary> = Vec::new();

    // Add windows in preferred order first, then any remaining
    for &label in &label_order {
        if let Some(entries) = groups.remove(label) {
            windows.push(build_pool_window(label.to_string(), &entries));
        }
    }
    // Any remaining labels not in our preferred order
    for (label, entries) in groups {
        windows.push(build_pool_window(label, &entries));
    }

    if windows.is_empty() {
        return None;
    }

    // Traffic light: based on worst avg across windows
    let max_avg = windows.iter().map(|w| w.avg_pct).max().unwrap_or(0);
    let (pool_status, status_text) = if max_avg >= 80 {
        ("red".to_string(), "Constrained \u{2014} defer big jobs".to_string())
    } else if max_avg >= 50 {
        ("yellow".to_string(), "Moderate \u{2014} light work preferred".to_string())
    } else {
        ("green".to_string(), "All clear \u{2014} launch heavy jobs".to_string())
    };

    let total_accounts = rows.iter().filter(|r| !r.paused).count();
    let available_accounts = rows
        .iter()
        .filter(|r| {
            !r.paused && r.rate_limit_status == "OK"
        })
        .count();

    Some(PoolUsageSummary {
        windows,
        total_accounts,
        available_accounts,
        pool_status,
        status_text,
    })
}

/// Build a single pool window summary from collected (pct, reset_text) pairs.
fn build_pool_window(label: String, entries: &[(i64, String)]) -> PoolWindowSummary {
    let count = entries.len();
    let sum: i64 = entries.iter().map(|(p, _)| *p).sum();
    let avg = sum / count as i64;
    let max = entries.iter().map(|(p, _)| *p).max().unwrap_or(0);

    // Pick earliest non-empty reset text
    let next_reset = entries
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(_, r)| r.as_str())
        .min_by(|a, b| {
            // Simple heuristic: shorter reset texts are sooner
            // "resetting" < "5m" < "2h 15m" < "3d 2h"
            parse_reset_minutes(a).cmp(&parse_reset_minutes(b))
        })
        .unwrap_or("")
        .to_string();

    PoolWindowSummary {
        label,
        avg_pct: avg,
        max_pct: max,
        css_class: utilization_class(avg),
        account_count: count,
        next_reset,
    }
}

/// Parse a reset text like "2h 15m", "45m", "resetting" into approximate minutes for sorting.
fn parse_reset_minutes(s: &str) -> i64 {
    if s == "resetting" {
        return 0;
    }
    let mut total = 0i64;
    for part in s.split_whitespace() {
        if let Some(h) = part.strip_suffix('h') {
            total += h.parse::<i64>().unwrap_or(0) * 60;
        } else if let Some(m) = part.strip_suffix('m') {
            total += m.parse::<i64>().unwrap_or(0);
        } else if let Some(d) = part.strip_suffix('d') {
            total += d.parse::<i64>().unwrap_or(0) * 1440;
        }
    }
    total
}

/// Build usage window display data from the UsageCache.
fn build_usage_windows(
    cache: Option<&bccf_providers::UsageCache>,
    account_id: &str,
    now: i64,
) -> Vec<UsageWindowDisplay> {
    use bccf_providers::usage_polling::AnyUsageData;

    let Some(cache) = cache else {
        return Vec::new();
    };
    let Some(data) = cache.get(account_id) else {
        return Vec::new();
    };

    let mut windows = Vec::new();

    match &data {
        AnyUsageData::Anthropic(map) => {
            // Show each known window from the Anthropic usage API.
            // The API returns utilization as a percentage (0-100).
            let window_order = [
                ("five_hour", "5-hour"),
                ("seven_day", "Weekly"),
                ("seven_day_opus", "Opus (Wk)"),
                ("seven_day_sonnet", "Sonnet (Wk)"),
            ];

            for (key, label) in window_order {
                if let Some(val) = map.get(key) {
                    if let Some(util) = val.get("utilization").and_then(|v| v.as_f64()) {
                        let resets_at = val
                            .get("resets_at")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let pct = util.round() as i64;
                        windows.push(UsageWindowDisplay {
                            label: label.to_string(),
                            pct: pct.clamp(0, 100),
                            css_class: utilization_class(pct),
                            reset_text: format_reset_time(&resets_at, now),
                        });
                    }
                }
            }
        }
        AnyUsageData::NanoGpt(data) => {
            if data.active {
                let daily_pct = (data.daily.percent_used * 100.0).round() as i64;
                windows.push(UsageWindowDisplay {
                    label: "Daily".to_string(),
                    pct: daily_pct.min(100),
                    css_class: utilization_class(daily_pct),
                    reset_text: format_reset_timestamp(data.daily.reset_at, now),
                });
                let monthly_pct = (data.monthly.percent_used * 100.0).round() as i64;
                windows.push(UsageWindowDisplay {
                    label: "Monthly".to_string(),
                    pct: monthly_pct.min(100),
                    css_class: utilization_class(monthly_pct),
                    reset_text: format_reset_timestamp(data.monthly.reset_at, now),
                });
            }
        }
        AnyUsageData::Zai(data) => {
            if let Some(ref tl) = data.tokens_limit {
                let pct = tl.percentage.round() as i64;
                windows.push(UsageWindowDisplay {
                    label: "5-hour".to_string(),
                    pct: pct.min(100),
                    css_class: utilization_class(pct),
                    reset_text: data
                        .tokens_limit
                        .as_ref()
                        .and_then(|t| t.reset_at)
                        .map(|ts| format_reset_timestamp(ts, now))
                        .unwrap_or_default(),
                });
            }
        }
    }

    windows
}

/// Predict which account the load balancer would choose next.
///
/// Mirrors the SessionStrategy logic (without side effects like round-robin
/// counter increment or session resets):
/// 1. Auto-fallback candidates (anthropic + auto_fallback + rate_limit_reset passed)
/// 2. Active session (anthropic with session within 5h)
/// 3. Highest priority available account
fn predict_next_account(accounts: &[bccf_core::types::Account], now: i64) -> Option<String> {
    let session_duration_ms: i64 = 5 * 60 * 60 * 1000; // 5 hours

    let is_available = |a: &bccf_core::types::Account| -> bool {
        !a.paused && a.rate_limited_until.map_or(true, |until| until < now)
    };

    // 1. Auto-fallback candidates
    let mut fallback: Vec<_> = accounts
        .iter()
        .filter(|a| {
            a.auto_fallback_enabled
                && a.provider == "anthropic"
                && a.rate_limit_reset.is_some_and(|reset| reset < now - 1000)
                && is_available(a)
        })
        .collect();
    fallback.sort_by_key(|a| a.priority);
    if let Some(a) = fallback.first() {
        return Some(a.id.clone());
    }

    // 2. Active session (most recent)
    let active = accounts
        .iter()
        .filter(|a| {
            matches!(a.provider.as_str(), "anthropic" | "claude-oauth")
                && a.session_start
                    .is_some_and(|start| now - start < session_duration_ms)
                && is_available(a)
        })
        .max_by_key(|a| a.session_start.unwrap_or(0));
    if let Some(a) = active {
        return Some(a.id.clone());
    }

    // 3. Highest priority available
    let mut available: Vec<_> = accounts.iter().filter(|a| is_available(a)).collect();
    available.sort_by_key(|a| a.priority);
    available.first().map(|a| a.id.clone())
}

/// CSS class for utilization percentage.
fn utilization_class(pct: i64) -> String {
    if pct >= 80 {
        "danger".to_string()
    } else if pct >= 50 {
        "warning".to_string()
    } else {
        "success".to_string()
    }
}

/// Format an ISO reset time string as a relative duration.
fn format_reset_time(iso: &Option<String>, now: i64) -> String {
    let Some(iso_str) = iso else {
        return String::new();
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso_str) else {
        return String::new();
    };
    let reset_ms = dt.timestamp_millis();
    let remaining_ms = reset_ms - now;
    if remaining_ms <= 0 {
        return "resetting".to_string();
    }
    let mins = remaining_ms / 60_000;
    let hours = mins / 60;
    let m = mins % 60;
    if hours > 0 {
        format!("{hours}h {m}m")
    } else {
        format!("{mins}m")
    }
}

/// Format a Unix timestamp (ms) as relative duration.
fn format_reset_timestamp(ts: i64, now: i64) -> String {
    let remaining_ms = ts - now;
    if remaining_ms <= 0 {
        return "resetting".to_string();
    }
    let mins = remaining_ms / 60_000;
    let hours = mins / 60;
    let m = mins % 60;
    if hours > 0 {
        format!("{hours}h {m}m")
    } else {
        format!("{mins}m")
    }
}

/// Query parameters for the requests table partial.
#[derive(Debug, Deserialize)]
struct RequestsTableQuery {
    #[serde(default = "default_page")]
    page: i64,
    account: Option<String>,
    model: Option<String>,
    project: Option<String>,
}

fn default_page() -> i64 {
    1
}

const REQUESTS_PER_PAGE: i64 = 50;

/// GET /dashboard/partials/requests-table — paginated, filterable requests table.
async fn requests_table_partial(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RequestsTableQuery>,
) -> Response {
    let now = chrono::Utc::now().timestamp_millis();

    let Some(pool) = state.db_pool::<DbPool>() else {
        return render_empty_requests();
    };
    let Ok(conn) = pool.get() else {
        return render_empty_requests();
    };

    // Build dynamic WHERE clause for filters
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(ref acct) = query.account {
        if !acct.is_empty() {
            conditions.push(format!(
                "r.account_used IN (SELECT id FROM accounts WHERE name LIKE ?{idx})"
            ));
            params.push(Box::new(format!("%{acct}%")));
            idx += 1;
        }
    }
    if let Some(ref model) = query.model {
        if !model.is_empty() {
            conditions.push(format!("r.model LIKE ?{idx}"));
            params.push(Box::new(format!("%{model}%")));
            idx += 1;
        }
    }
    if let Some(ref project) = query.project {
        if !project.is_empty() {
            conditions.push(format!("r.project LIKE ?{idx}"));
            params.push(Box::new(format!("%{project}%")));
            idx += 1;
        }
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    // Count total matching rows
    let count_sql = format!("SELECT COUNT(*) FROM requests r {where_clause}");
    let total: i64 = match conn.query_row(
        &count_sql,
        rusqlite::params_from_iter(params.iter()),
        |row| row.get(0),
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to count requests: {e}");
            return render_empty_requests();
        }
    };

    let page = query.page.max(1);
    let total_pages = ((total + REQUESTS_PER_PAGE - 1) / REQUESTS_PER_PAGE).max(1);
    let offset = (page - 1) * REQUESTS_PER_PAGE;

    // Fetch page with account name join
    let select_sql = format!(
        "SELECT r.*, COALESCE(a.name, r.account_used) as account_name
         FROM requests r
         LEFT JOIN accounts a ON a.id = r.account_used
         {where_clause}
         ORDER BY r.timestamp DESC
         LIMIT ?{idx} OFFSET ?{}",
        idx + 1
    );
    params.push(Box::new(REQUESTS_PER_PAGE));
    params.push(Box::new(offset));

    let result: Result<Vec<RequestRow>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare(&select_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let timestamp: i64 = row.get("timestamp")?;
            let diff_ms = now - timestamp;
            let secs = diff_ms / 1000;
            let timestamp_relative = if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86400)
            };

            let model_full: Option<String> = row.get("model")?;
            let model_short = model_full
                .as_deref()
                .map(bccf_core::models::get_model_short_name)
                .unwrap_or("unknown")
                .to_string();

            let response_time_ms: Option<i64> = row.get("response_time_ms")?;
            let response_time_display = response_time_ms.map(|ms| {
                if ms >= 1000 {
                    format!("{:.1}s", ms as f64 / 1000.0)
                } else {
                    format!("{ms}ms")
                }
            });

            let cost_usd: Option<f64> = row.get("cost_usd")?;
            let cost_display = cost_usd.map(|c| {
                if c >= 0.01 {
                    format!("{c:.4}")
                } else {
                    format!("{c:.6}")
                }
            });

            let account_name: Option<String> = row.get("account_name")?;

            Ok(RequestRow {
                id: row.get("id")?,
                timestamp_relative,
                account_name: account_name.unwrap_or_default(),
                model_short,
                status_code: row.get::<_, Option<i64>>("status_code")?.unwrap_or(0),
                success: row.get::<_, i64>("success")? != 0,
                input_tokens: row.get::<_, Option<i64>>("input_tokens")?.unwrap_or(0),
                output_tokens: row.get::<_, Option<i64>>("output_tokens")?.unwrap_or(0),
                total_tokens: row.get::<_, Option<i64>>("total_tokens")?.unwrap_or(0),
                response_time_display,
                cost_display,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })();

    match result {
        Ok(rows) => {
            let tpl = RequestsTablePartial {
                requests: rows,
                page,
                total_pages,
                total,
            };
            match tpl.render() {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                    tracing::error!("Requests table render error: {e}");
                    Html("<p>Error rendering requests table</p>".to_string()).into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to fetch requests: {e}");
            render_empty_requests()
        }
    }
}

/// Render an empty requests table when DB is unavailable.
fn render_empty_requests() -> Response {
    let tpl = RequestsTablePartial {
        requests: Vec::new(),
        page: 1,
        total_pages: 1,
        total: 0,
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => Html("<p>No requests recorded yet.</p>".to_string()).into_response(),
    }
}

/// Format a timestamp as a relative time string.
fn format_relative_time(now: i64, ts: i64) -> String {
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
}

/// GET /dashboard/partials/agents-table — HTMX partial for agents table.
async fn agents_table_partial(State(state): State<Arc<AppState>>) -> Response {
    let now = chrono::Utc::now().timestamp_millis();
    let default_model = bccf_core::DEFAULT_AGENT_MODEL.to_string();

    let agents = match state.db_pool::<DbPool>() {
        Some(pool) => match pool.get() {
            Ok(conn) => bccf_database::repositories::agent_preference::get_all_preferences(&conn)
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };

    let rows: Vec<AgentRow> = agents
        .into_iter()
        .map(|a| {
            let model_options = bccf_core::models::ALL_MODEL_IDS
                .iter()
                .map(|&m| ModelOption {
                    id: m.to_string(),
                    selected: m == a.preferred_model,
                    is_default: m == default_model,
                })
                .collect();
            AgentRow {
                agent_id: a.agent_id.clone(),
                preferred_model: a.preferred_model,
                model_options,
                updated_at_relative: format_relative_time(now, a.updated_at),
            }
        })
        .collect();

    let tpl = AgentsTablePartial {
        agents: rows,
        default_model,
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Agents table render error: {e}");
            Html("<p>Error rendering agents table</p>".to_string()).into_response()
        }
    }
}

/// GET /dashboard/partials/api-keys-table — HTMX partial for API keys table.
async fn api_keys_table_partial(State(state): State<Arc<AppState>>) -> Response {
    let now = chrono::Utc::now().timestamp_millis();

    let keys_data = match state.db_pool::<DbPool>() {
        Some(pool) => match pool.get() {
            Ok(conn) => bccf_database::repositories::api_key::find_all(&conn).unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };

    let total = keys_data.len() as i64;
    let active = keys_data.iter().filter(|k| k.is_active).count() as i64;

    let rows: Vec<ApiKeyRow> = keys_data
        .into_iter()
        .map(|k| {
            let created_relative = format_relative_time(now, k.created_at);
            let last_used_relative = k.last_used.map(|ts| format_relative_time(now, ts));
            ApiKeyRow {
                id: k.id,
                name: k.name,
                prefix_last_8: k.prefix_last_8,
                created_at_relative: created_relative,
                last_used_relative,
                usage_count: k.usage_count,
                is_active: k.is_active,
            }
        })
        .collect();

    let tpl = ApiKeysTablePartial {
        keys: rows,
        total,
        active,
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("API keys table render error: {e}");
            Html("<p>Error rendering API keys table</p>".to_string()).into_response()
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
        "chart.min.js" => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/javascript"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            CHART_JS,
        )
            .into_response(),
        "favicon.svg" => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "public, max-age=604800"),
            ],
            FAVICON_SVG,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Stats table partial
// ---------------------------------------------------------------------------

/// GET /dashboard/partials/stats-table — render the stats table.
async fn stats_table_partial(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return render_empty_stats();
    };
    let Ok(conn) = pool.get() else {
        return render_empty_stats();
    };

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

    let accounts: Vec<StatsAccountRow> =
        bccf_database::repositories::stats::get_account_stats(&conn, 20)
            .unwrap_or_default()
            .into_iter()
            .map(|a| StatsAccountRow {
                name: a.name,
                request_count: a.request_count,
                success_rate: a.success_rate,
            })
            .collect();

    let top_models: Vec<StatsModelRow> =
        bccf_database::repositories::stats::get_top_models(&conn, 10)
            .unwrap_or_default()
            .into_iter()
            .map(|m| StatsModelRow {
                name: m.model,
                count: m.count,
                percentage: m.percentage,
            })
            .collect();

    let recent_errors =
        bccf_database::repositories::stats::get_recent_errors(&conn, 10).unwrap_or_default();

    let tpl = StatsTablePartial {
        total_requests: aggregated.total_requests,
        success_rate,
        avg_response_time: aggregated.avg_response_time,
        total_cost_usd: aggregated.total_cost_usd,
        accounts,
        top_models,
        recent_errors,
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Stats table render error: {e}");
            Html("<p>Error rendering stats table</p>".to_string()).into_response()
        }
    }
}

/// Render an empty stats table when DB is unavailable.
fn render_empty_stats() -> Response {
    let tpl = StatsTablePartial {
        total_requests: 0,
        success_rate: 0.0,
        avg_response_time: 0.0,
        total_cost_usd: 0.0,
        accounts: Vec::new(),
        top_models: Vec::new(),
        recent_errors: Vec::new(),
    };
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(_) => Html("<p>No statistics available yet.</p>".to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Logs stream partial
// ---------------------------------------------------------------------------

/// GET /dashboard/partials/logs-stream — render the logs stream UI.
async fn logs_stream_partial() -> Response {
    let tpl = LogsStreamPartial;
    match tpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Logs stream render error: {e}");
            Html("<p>Error rendering log stream</p>".to_string()).into_response()
        }
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
    use bccf_core::AppStateBuilder;
    use bccf_database::pool::{create_memory_pool, PoolConfig};
    use http::Request;
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-dashboard-nonexistent/config.json",
        )))
        .unwrap();
        Arc::new(AppState::new(config))
    }

    fn test_state_with_db() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-dashboard-nonexistent/config.json",
        )))
        .unwrap();
        let pool = create_memory_pool(&PoolConfig::default()).unwrap();
        let state = AppStateBuilder::new(config).db_pool(pool).build();
        Arc::new(state)
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
        let state = test_state_with_db();
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
