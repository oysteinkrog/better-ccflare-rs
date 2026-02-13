//! Log streaming and history API handlers.
//!
//! - `GET /api/logs/stream` — SSE: real-time log entries
//! - `GET /api/logs` — historical log entries

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::json;

use bccf_core::AppState;

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub level: Option<String>,
    #[serde(default = "default_log_limit")]
    pub limit: usize,
}

fn default_log_limit() -> usize {
    100
}

// ---------------------------------------------------------------------------
// GET /api/logs/stream
// ---------------------------------------------------------------------------

/// SSE stream of real-time log entries.
pub async fn logs_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_bus.subscribe();

    let stream = async_stream::stream! {
        // Send initial connected event
        yield Ok(Event::default().data(json!({"connected": true}).to_string()));

        loop {
            match rx.recv().await {
                Ok(json) => {
                    // Filter to log_entry events only
                    if json.contains("\"type\":\"log_entry\"") {
                        yield Ok(Event::default().data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Log stream subscriber lagged by {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// GET /api/logs
// ---------------------------------------------------------------------------

/// Historical log entries (stub — log file reader not yet ported).
///
/// Returns an empty array until the log file writer is implemented in Rust.
pub async fn logs_history(Query(_query): Query<LogsQuery>) -> impl IntoResponse {
    // TODO: implement log file reader when available
    Json(json!({
        "logs": [],
        "message": "Log file reader not yet available in Rust build"
    }))
}
