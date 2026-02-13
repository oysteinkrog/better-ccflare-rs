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

/// The 8 dashboard tabs, in display order.
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
}

// ---------------------------------------------------------------------------
// Tab fragment templates (HTMX partial responses)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "tabs/overview.html")]
pub struct OverviewTab;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_count() {
        assert_eq!(TABS.len(), 8);
    }

    #[test]
    fn overview_renders() {
        let tpl = OverviewTab;
        let html = tpl.render().unwrap();
        assert!(html.contains("Overview"));
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
        };
        let html = tpl.render().unwrap();
        assert!(html.contains("better-ccflare"));
        assert!(html.contains("htmx.min.js"));
        assert!(html.contains("pico.min.css"));
        assert!(html.contains("Overview</h2>"));
    }
}
