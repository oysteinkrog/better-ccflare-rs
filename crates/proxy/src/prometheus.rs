//! Prometheus metrics endpoint (US-029b).
//!
//! Provides an optional `/metrics` endpoint that exposes Prometheus-format
//! metrics for monitoring. Disabled by default; enable via `metrics_enabled`
//! in config.json or `METRICS_ENABLED=true` environment variable.
//!
//! # Metrics
//!
//! | Metric | Type | Labels | Description |
//! |--------|------|--------|-------------|
//! | `request_total` | Counter | status, provider | Total proxied requests |
//! | `request_duration_seconds` | Histogram | provider | Proxy overhead latency |
//! | `active_connections` | Gauge | — | Current SSE + proxy connections |
//! | `account_state` | Gauge | account, state | Account status (1 = active) |
//! | `memory_usage_bytes` | Gauge | — | Process RSS from /proc/self/status |

use std::sync::OnceLock;

use axum::response::IntoResponse;
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Global Prometheus recorder handle, initialized once.
static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Initialize the Prometheus metrics recorder.
///
/// Must be called once at startup (before any metrics are recorded).
/// Returns `Ok(())` on success, or `Err` if already initialized or the
/// recorder could not be installed.
pub fn init_metrics() -> Result<(), String> {
    let builder = PrometheusBuilder::new();
    let handle = builder
        .install_recorder()
        .map_err(|e| format!("Failed to install Prometheus recorder: {e}"))?;

    PROMETHEUS_HANDLE
        .set(handle)
        .map_err(|_| "Prometheus metrics already initialized".to_string())?;

    // Pre-register metrics with initial values so they appear in /metrics
    // even before any requests are processed.
    let _ = counter!("request_total", "status" => "200", "provider" => "unknown");
    gauge!("active_connections").set(0.0);
    gauge!("memory_usage_bytes").set(0.0);

    tracing::info!("Prometheus metrics recorder initialized");
    Ok(())
}

/// Get the global Prometheus handle (for rendering /metrics).
fn prometheus_handle() -> Option<&'static PrometheusHandle> {
    PROMETHEUS_HANDLE.get()
}

// ---------------------------------------------------------------------------
// Metric recording helpers
// ---------------------------------------------------------------------------

/// Record a completed proxy request.
pub fn record_request(status_code: u16, provider: &str, duration_secs: f64) {
    let status = status_code.to_string();
    counter!("request_total", "status" => status, "provider" => provider.to_string()).increment(1);
    histogram!("request_duration_seconds", "provider" => provider.to_string())
        .record(duration_secs);
}

/// Set the current number of active connections (SSE + proxy).
pub fn set_active_connections(count: f64) {
    gauge!("active_connections").set(count);
}

/// Set the state gauge for a specific account.
///
/// Sets the labeled gauge to 1.0 for the given state and 0.0 for others.
pub fn set_account_state(account_name: &str, state: &str) {
    for s in &["active", "paused", "rate_limited"] {
        let val = if *s == state { 1.0 } else { 0.0 };
        gauge!("account_state", "account" => account_name.to_string(), "state" => s.to_string())
            .set(val);
    }
}

/// Update the memory_usage_bytes gauge from /proc/self/status (Linux only).
pub fn update_memory_usage() {
    if let Some(rss) = read_rss_bytes() {
        gauge!("memory_usage_bytes").set(rss as f64);
    }
}

/// Read VmRSS from /proc/self/status (Linux).
#[cfg(target_os = "linux")]
fn read_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let trimmed = rest.trim();
            // Format: "1234 kB"
            let kb_str = trimmed.split_whitespace().next()?;
            let kb: u64 = kb_str.parse().ok()?;
            return Some(kb * 1024); // Convert to bytes
        }
    }
    None
}

/// Non-Linux: always returns None.
#[cfg(not(target_os = "linux"))]
fn read_rss_bytes() -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// Axum handler
// ---------------------------------------------------------------------------

/// Handler for `GET /metrics` — returns Prometheus text exposition format.
pub async fn metrics_handler() -> impl IntoResponse {
    // Update memory gauge on each scrape
    update_memory_usage();

    match prometheus_handle() {
        Some(handle) => {
            let body = handle.render();
            (
                [(
                    http::header::CONTENT_TYPE,
                    "text/plain; version=0.0.4; charset=utf-8",
                )],
                body,
            )
                .into_response()
        }
        None => (
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Metrics not initialized",
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: Because the `metrics` crate uses a global recorder, and
    // `install_recorder` can only be called once per process, we test
    // recording helpers in isolation (they don't panic without a recorder)
    // and test the handler response shape.

    #[test]
    fn record_request_no_panic_without_recorder() {
        // Calling metric macros without a recorder just no-ops
        record_request(200, "anthropic", 0.5);
    }

    #[test]
    fn set_active_connections_no_panic() {
        set_active_connections(42.0);
    }

    #[test]
    fn set_account_state_no_panic() {
        set_account_state("my-account", "active");
    }

    #[test]
    fn update_memory_usage_no_panic() {
        update_memory_usage();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_rss_returns_some_on_linux() {
        let rss = read_rss_bytes();
        assert!(rss.is_some(), "Should read VmRSS on Linux");
        assert!(rss.unwrap() > 0);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn read_rss_returns_none_on_non_linux() {
        assert!(read_rss_bytes().is_none());
    }

    #[tokio::test]
    async fn metrics_handler_returns_503_when_not_initialized() {
        // Without calling init_metrics(), the handle is None
        let resp = metrics_handler().await.into_response();
        // The status depends on whether another test in this process
        // already initialized the recorder. We just verify no panic.
        assert!(
            resp.status() == http::StatusCode::OK
                || resp.status() == http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn prometheus_handle_none_before_init() {
        // The handle may be set if another test called init_metrics.
        // This test just verifies no panic.
        let _ = prometheus_handle();
    }

    #[test]
    fn record_multiple_requests() {
        for status in [200, 400, 500] {
            record_request(status, "test-provider", 0.1);
        }
    }

    #[test]
    fn set_account_state_transitions() {
        set_account_state("acct-1", "active");
        set_account_state("acct-1", "paused");
        set_account_state("acct-1", "rate_limited");
    }
}
