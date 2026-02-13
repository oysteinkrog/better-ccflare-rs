//! Real-time SSE streaming handler for request events.
//!
//! `GET /api/requests/stream` — emits RequestStart and RequestSummary events.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;

use bccf_core::AppState;

// ---------------------------------------------------------------------------
// GET /api/requests/stream
// ---------------------------------------------------------------------------

/// SSE stream of real-time request events (start, summary).
pub async fn request_events_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.event_bus.subscribe();

    let stream = async_stream::stream! {
        // Send initial connected event
        yield Ok(Event::default().event("connected").data("ok"));

        loop {
            match rx.recv().await {
                Ok(json) => {
                    // Filter to only request events
                    if json.contains("\"type\":\"request_start\"")
                        || json.contains("\"type\":\"request_summary\"")
                    {
                        yield Ok(Event::default().data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Request stream subscriber lagged by {n} events");
                    // Continue receiving after lag
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
