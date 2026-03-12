//! Askama template structs.
//!
//! Each struct maps to a template file in the `templates/` directory.
//! Askama compiles these at build time for zero-cost rendering.

use askama::Template;

/// Metadata for a navigation tab.
pub struct TabInfo {
    pub slug: &'static str,
    pub label: &'static str,
}

/// The 9 dashboard tabs, in display order.
pub const TABS: &[TabInfo] = &[
    TabInfo {
        slug: "overview",
        label: "Overview",
    },
    TabInfo {
        slug: "accounts",
        label: "Accounts",
    },
    TabInfo {
        slug: "requests",
        label: "Requests",
    },
    TabInfo {
        slug: "analytics",
        label: "Analytics",
    },
    TabInfo {
        slug: "capacity",
        label: "Capacity",
    },
    TabInfo {
        slug: "stats",
        label: "Stats",
    },
    TabInfo {
        slug: "logs",
        label: "Logs",
    },
    TabInfo {
        slug: "agents",
        label: "Agents",
    },
    TabInfo {
        slug: "api-keys",
        label: "API Keys",
    },
];

// ---------------------------------------------------------------------------
// Full-page templates (base layout + tab content)
// ---------------------------------------------------------------------------

/// Full page: base layout wrapping a tab's content.
/// Used for direct URL access (non-HTMX requests).
#[derive(Template)]
#[template(path = "base.html")]
pub struct BasePage<'a> {
    pub version: &'a str,
    pub tabs: &'a [TabInfo],
    pub active_tab: &'a str,
    pub tab_content: &'a str,
    pub dashboard_api_key: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Tab fragment templates (HTMX partial responses)
// ---------------------------------------------------------------------------

/// Overview tab data — server-rendered stats cards.
#[derive(Template)]
#[template(path = "tabs/overview.html")]
pub struct OverviewTab {
    pub total_requests: i64,
    pub success_rate: f64,
    pub avg_response_time: f64,
    pub total_cost_usd: f64,
    pub avg_tokens_per_second: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
    pub total_accounts: usize,
    pub active_accounts: i64,
    pub paused_accounts: usize,
    pub rate_limited_accounts: usize,
    pub healthy_accounts: usize,
    pub recent_errors: Vec<String>,
    pub top_models: Vec<OverviewModel>,
    pub version: String,
}

/// Model entry for the overview tab.
pub struct OverviewModel {
    pub name: String,
    pub count: i64,
    pub percentage: f64,
}

impl OverviewTab {
    /// Format an integer with comma separators (e.g. 1234 → "1,234").
    pub fn fmt_int(&self, n: &i64) -> String {
        let s = n.to_string();
        let bytes = s.as_bytes();
        let mut result = String::with_capacity(s.len() + s.len() / 3);
        for (i, &b) in bytes.iter().enumerate() {
            if i > 0 && (bytes.len() - i) % 3 == 0 {
                result.push(',');
            }
            result.push(b as char);
        }
        result
    }

    /// Format a percentage to 1 decimal place (e.g. 95.5).
    pub fn fmt_pct(&self, v: &f64) -> String {
        format!("{v:.1}")
    }

    /// Format milliseconds (e.g. "245ms" or "1.2s").
    pub fn fmt_ms(&self, v: &f64) -> String {
        if *v >= 1000.0 {
            format!("{:.1}s", v / 1000.0)
        } else {
            format!("{:.0}ms", v)
        }
    }

    /// Format USD cost (e.g. "12.50" or "0.0015").
    pub fn fmt_usd(&self, v: &f64) -> String {
        if *v >= 1.0 {
            format!("{v:.2}")
        } else if *v >= 0.01 {
            format!("{v:.4}")
        } else {
            format!("{v:.6}")
        }
    }

    /// Format tokens/second speed.
    pub fn fmt_speed(&self, v: &f64) -> String {
        format!("{v:.1}")
    }
}

#[derive(Template)]
#[template(path = "tabs/accounts.html")]
pub struct AccountsTab;

#[derive(Template)]
#[template(path = "tabs/requests.html")]
pub struct RequestsTab;

#[derive(Template)]
#[template(path = "tabs/analytics.html")]
pub struct AnalyticsTab;

#[derive(Template)]
#[template(path = "tabs/stats.html")]
pub struct StatsTab;

#[derive(Template)]
#[template(path = "tabs/logs.html")]
pub struct LogsTab;

#[derive(Template)]
#[template(path = "tabs/agents.html")]
pub struct AgentsTab;

#[derive(Template)]
#[template(path = "tabs/api_keys.html")]
pub struct ApiKeysTab;

#[derive(Template)]
#[template(path = "tabs/capacity.html")]
pub struct CapacityTab;

// ---------------------------------------------------------------------------
// Partial templates (HTMX partials for dynamic content)
// ---------------------------------------------------------------------------

/// A usage window with utilization percentage and optional reset time.
pub struct UsageWindowDisplay {
    /// Window label for display (e.g. "5-hour", "Weekly", "Opus (Weekly)").
    pub label: String,
    /// Utilization percentage (0-100), or -1 if not available.
    pub pct: i64,
    /// CSS class for the progress bar ("success", "warning", "danger").
    pub css_class: String,
    /// Human-readable time until reset (e.g. "2h 15m"), empty if unknown.
    pub reset_text: String,
    /// Unix timestamp (ms) when this window resets; used by chart visualizations.
    pub resets_at_ms: Option<i64>,
}

/// A single account row in the accounts table partial.
pub struct AccountRow {
    pub id: String,
    pub name: String,
    pub provider: String,
    /// First character of provider name, uppercased (for fallback icon).
    pub provider_initial: String,
    pub priority: i64,
    pub paused: bool,
    pub auto_fallback_enabled: bool,
    pub token_status_str: String,
    /// True when the latest provider status indicates auth failure (401/403/re-auth needed).
    pub auth_failed: bool,
    pub rate_limit_status: String,
    pub session_info: String,
    pub request_count: i64,
    pub total_requests: i64,
    pub last_used_relative: Option<String>,
    /// Raw last-used timestamp (ms) for sort-by-last-used; not displayed in template.
    pub last_used_ms: Option<i64>,
    /// Raw session_start timestamp (ms) — used for LB-order sort; not displayed.
    pub session_start_ms: Option<i64>,
    /// Raw rate_limit_reset timestamp (ms) — used for LB-order sort; not displayed.
    pub rate_limit_reset_ms: Option<i64>,
    /// Soonest window reset timestamp (ms) from usage — used for LB-order sort; not displayed.
    pub resets_at_ms: Option<i64>,
    pub custom_endpoint: Option<String>,
    /// Per-window usage data from provider API (empty if provider doesn't support it).
    pub usage_windows: Vec<UsageWindowDisplay>,
    /// Whether this account supports usage tracking.
    pub has_usage: bool,
    pub is_oauth: bool,
    /// Whether the load balancer would choose this account next.
    pub is_next: bool,
    /// 5-hour reserve capacity percentage (0-100).
    pub reserve_5h: i64,
    /// Weekly reserve capacity percentage (0-100).
    pub reserve_weekly: i64,
    /// Whether reserve is hard (excluded) or soft (deprioritized).
    pub reserve_hard: bool,
    /// Human-readable subscription tier for OAuth accounts (e.g. "Max 20x", "Pro").
    pub subscription_tier: Option<String>,
    /// Email address of the authenticated OAuth user.
    pub email: Option<String>,
    /// Whether overage protection is enabled (skip account at 100% usage).
    pub overage_protection: bool,
    /// Whether the account is currently excluded by overage protection.
    pub overage_blocked: bool,
}

/// Aggregate pool usage summary shown above account cards.
pub struct PoolUsageSummary {
    pub windows: Vec<PoolWindowSummary>,
    pub total_accounts: usize,
    pub available_accounts: usize,
    pub pool_status: String,
    pub status_text: String,
}

/// One window in the pool usage summary (e.g. "5-hour", "Weekly").
pub struct PoolWindowSummary {
    pub label: String,
    pub avg_pct: i64,
    pub max_pct: i64,
    pub css_class: String,
    pub account_count: usize,
    pub next_reset: String,
}

/// Accounts table partial — rendered by `/dashboard/partials/accounts-table`.
#[derive(Template)]
#[template(path = "partials/accounts_table.html")]
pub struct AccountsTablePartial {
    pub accounts: Vec<AccountRow>,
    pub pool_summary: Option<PoolUsageSummary>,
    /// Pre-serialized JSON for quota visualization charts (safe to embed in script tag).
    pub chart_data_json: String,
}

/// A single account stats entry for the stats table partial.
pub struct StatsAccountRow {
    pub name: String,
    pub request_count: i64,
    pub success_rate: f64,
}

/// A top model entry for the stats table partial.
pub struct StatsModelRow {
    pub name: String,
    pub count: i64,
    pub percentage: f64,
}

/// Stats table partial — rendered by `/dashboard/partials/stats-table`.
#[derive(Template)]
#[template(path = "partials/stats_table.html")]
pub struct StatsTablePartial {
    pub total_requests: i64,
    pub success_rate: f64,
    pub avg_response_time: f64,
    pub total_cost_usd: f64,
    pub accounts: Vec<StatsAccountRow>,
    pub top_models: Vec<StatsModelRow>,
    pub recent_errors: Vec<String>,
}

impl StatsTablePartial {
    pub fn fmt_int(&self, n: &i64) -> String {
        let s = n.to_string();
        let bytes = s.as_bytes();
        let mut result = String::with_capacity(s.len() + s.len() / 3);
        for (i, &b) in bytes.iter().enumerate() {
            if i > 0 && (bytes.len() - i) % 3 == 0 {
                result.push(',');
            }
            result.push(b as char);
        }
        result
    }

    pub fn fmt_pct(&self, v: &f64) -> String {
        format!("{v:.1}")
    }

    pub fn fmt_ms(&self, v: &f64) -> String {
        if *v >= 1000.0 {
            format!("{:.1}s", v / 1000.0)
        } else {
            format!("{:.0}ms", v)
        }
    }

    pub fn fmt_usd(&self, v: &f64) -> String {
        if *v >= 1.0 {
            format!("{v:.2}")
        } else if *v >= 0.01 {
            format!("{v:.4}")
        } else {
            format!("{v:.6}")
        }
    }
}

/// Logs stream partial — rendered by `/dashboard/partials/logs-stream`.
#[derive(Template)]
#[template(path = "partials/logs_stream.html")]
pub struct LogsStreamPartial;

/// A single request row in the requests table partial.
pub struct RequestRow {
    pub id: String,
    pub timestamp_relative: String,
    pub account_name: String,
    pub model_short: String,
    pub status_code: i64,
    pub success: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub response_time_display: Option<String>,
    pub cost_display: Option<String>,
}

/// Requests table partial — rendered by `/dashboard/partials/requests-table`.
#[derive(Template)]
#[template(path = "partials/requests_table.html")]
pub struct RequestsTablePartial {
    pub requests: Vec<RequestRow>,
    pub page: i64,
    pub total_pages: i64,
    pub total: i64,
}

// ---------------------------------------------------------------------------
// Agents table partial
// ---------------------------------------------------------------------------

/// A model option for a select dropdown, with pre-computed flags.
pub struct ModelOption {
    pub id: String,
    pub selected: bool,
    pub is_default: bool,
}

/// A single agent row in the agents table partial.
pub struct AgentRow {
    pub agent_id: String,
    pub preferred_model: String,
    pub model_options: Vec<ModelOption>,
    pub updated_at_relative: String,
}

/// Agents table partial — rendered by `/dashboard/partials/agents-table`.
#[derive(Template)]
#[template(path = "partials/agents_table.html")]
pub struct AgentsTablePartial {
    pub agents: Vec<AgentRow>,
    pub default_model: String,
}

// ---------------------------------------------------------------------------
// API Keys table partial
// ---------------------------------------------------------------------------

/// A single API key row in the API keys table partial.
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub prefix_last_8: String,
    pub created_at_relative: String,
    pub last_used_relative: Option<String>,
    pub usage_count: i64,
    pub is_active: bool,
}

/// API Keys table partial — rendered by `/dashboard/partials/api-keys-table`.
#[derive(Template)]
#[template(path = "partials/api_keys_table.html")]
pub struct ApiKeysTablePartial {
    pub keys: Vec<ApiKeyRow>,
    pub total: i64,
    pub active: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_count() {
        assert_eq!(TABS.len(), 9);
    }

    #[test]
    fn overview_renders() {
        let tpl = OverviewTab {
            total_requests: 1234,
            success_rate: 95.5,
            avg_response_time: 245.3,
            total_cost_usd: 12.50,
            avg_tokens_per_second: 33.5,
            input_tokens: 5000,
            output_tokens: 3000,
            cache_read_tokens: 1000,
            cache_creation_tokens: 500,
            total_tokens: 9500,
            total_accounts: 5,
            active_accounts: 4,
            paused_accounts: 1,
            rate_limited_accounts: 0,
            healthy_accounts: 4,
            recent_errors: vec!["Test error".to_string()],
            top_models: vec![OverviewModel {
                name: "claude-3-opus".to_string(),
                count: 100,
                percentage: 75.0,
            }],
            version: "1.0.0".to_string(),
        };
        let html = tpl.render().unwrap();
        assert!(html.contains("Overview"));
        assert!(html.contains("1,234"));
        assert!(html.contains("95.5"));
    }

    #[test]
    fn accounts_renders() {
        let tpl = AccountsTab;
        let html = tpl.render().unwrap();
        assert!(html.contains("Accounts"));
    }

    #[test]
    fn base_page_renders() {
        let tpl = BasePage {
            version: "0.1.0",
            tabs: TABS,
            active_tab: "overview",
            tab_content: "<h2>Overview</h2>",
            dashboard_api_key: None,
        };
        let html = tpl.render().unwrap();
        assert!(html.contains("better-ccflare"));
        assert!(html.contains("htmx.min.js"));
        assert!(html.contains("pico.min.css"));
        assert!(html.contains("Overview</h2>"));
    }
}
