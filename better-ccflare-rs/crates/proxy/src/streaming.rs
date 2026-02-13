//! Response streaming with SSE parsing and analytics tee.
//!
//! Provides zero-copy stream teeing: the client receives the full stream
//! while the analytics side accumulates up to 1 MB of bytes for background
//! processing.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tracing::warn;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum stream duration before abort (5 minutes).
pub const STREAM_TIMEOUT_MS: u64 = 300_000;

/// Maximum time between chunks before abort (30 seconds).
pub const CHUNK_TIMEOUT_MS: u64 = 30_000;

/// Maximum bytes buffered for analytics (1 MB).
pub const ANALYTICS_MAX_BYTES: usize = 1_048_576;

// ---------------------------------------------------------------------------
// Analytics buffer
// ---------------------------------------------------------------------------

/// Accumulated analytics data from a tee'd stream.
#[derive(Debug)]
pub struct AnalyticsBuffer {
    /// Buffered chunks (up to ANALYTICS_MAX_BYTES total).
    pub chunks: Vec<Bytes>,
    /// Total bytes in buffer.
    pub total_bytes: usize,
    /// Whether the buffer hit the cap and stopped accumulating.
    pub truncated: bool,
}

impl AnalyticsBuffer {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            total_bytes: 0,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &Bytes) {
        if self.truncated {
            return;
        }
        if self.total_bytes + chunk.len() <= ANALYTICS_MAX_BYTES {
            self.chunks.push(chunk.clone());
            self.total_bytes += chunk.len();
        } else {
            // Buffer partial chunk to reach exactly the cap
            let remaining = ANALYTICS_MAX_BYTES - self.total_bytes;
            if remaining > 0 {
                self.chunks.push(chunk.slice(..remaining));
                self.total_bytes += remaining;
            }
            self.truncated = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Tee stream
// ---------------------------------------------------------------------------

/// Shared state for the analytics side of a tee'd stream.
struct TeeState {
    buffer: AnalyticsBuffer,
    stream_start: Instant,
    last_chunk: Instant,
    timed_out: bool,
}

pin_project! {
    /// A stream wrapper that forwards chunks to the client while accumulating
    /// a copy for analytics processing.
    ///
    /// When the stream completes (or times out), the analytics buffer is sent
    /// via a oneshot channel.
    pub struct TeeStream<S> {
        #[pin]
        inner: S,
        state: Arc<Mutex<TeeState>>,
        tx: Option<oneshot::Sender<AnalyticsBuffer>>,
        stream_timeout: Duration,
        chunk_timeout: Duration,
    }
}

impl<S> Stream for TeeStream<S>
where
    S: Stream<Item = Result<Bytes, Infallible>>,
{
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        match this.inner.poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let mut state = this.state.lock().unwrap();
                let now = Instant::now();

                // Check timeouts
                if now.duration_since(state.stream_start) > *this.stream_timeout {
                    if !state.timed_out {
                        warn!("Stream exceeded maximum duration, aborting analytics");
                        state.timed_out = true;
                    }
                } else if now.duration_since(state.last_chunk) > *this.chunk_timeout
                    && !state.timed_out
                {
                    warn!("Chunk timeout exceeded, aborting analytics");
                    state.timed_out = true;
                }

                state.last_chunk = now;
                if !state.timed_out {
                    state.buffer.push(&chunk);
                }
                drop(state);

                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                // Stream ended — send buffer
                let buffer = {
                    let mut state = this.state.lock().unwrap();
                    std::mem::replace(&mut state.buffer, AnalyticsBuffer::new())
                };
                if let Some(tx) = this.tx.take() {
                    let _ = tx.send(buffer);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

/// Create a tee'd stream that forwards all data to the client while buffering
/// up to `ANALYTICS_MAX_BYTES` for analytics processing.
///
/// Returns: (tee'd stream for client, receiver for analytics buffer)
pub fn tee_stream<S>(inner: S) -> (TeeStream<S>, oneshot::Receiver<AnalyticsBuffer>)
where
    S: Stream<Item = Result<Bytes, Infallible>>,
{
    let (tx, rx) = oneshot::channel();
    let now = Instant::now();
    let state = Arc::new(Mutex::new(TeeState {
        buffer: AnalyticsBuffer::new(),
        stream_start: now,
        last_chunk: now,
        timed_out: false,
    }));

    let stream = TeeStream {
        inner,
        state,
        tx: Some(tx),
        stream_timeout: Duration::from_millis(STREAM_TIMEOUT_MS),
        chunk_timeout: Duration::from_millis(CHUNK_TIMEOUT_MS),
    };

    (stream, rx)
}

// ---------------------------------------------------------------------------
// SSE Parser
// ---------------------------------------------------------------------------

/// A parsed SSE event.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

/// Stateful SSE line parser that handles chunks split across boundaries.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Incomplete line from previous chunk.
    line_buffer: String,
    /// Current event type (persists across data lines).
    current_event: Option<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes, returning any complete SSE events found.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let text = String::from_utf8_lossy(chunk);
        let mut events = Vec::new();

        // Prepend any leftover from previous chunk
        let combined = if self.line_buffer.is_empty() {
            text.into_owned()
        } else {
            let mut s = std::mem::take(&mut self.line_buffer);
            s.push_str(&text);
            s
        };

        let mut lines = combined.split('\n').peekable();
        while let Some(line) = lines.next() {
            if lines.peek().is_none() {
                // Last segment may be incomplete
                if !line.is_empty() {
                    self.line_buffer = line.to_string();
                }
                break;
            }

            let line = line.trim_end_matches('\r');

            if line.is_empty() {
                // Empty line = end of event (SSE spec)
                // Reset current_event for next event
                self.current_event = None;
                continue;
            }

            if let Some(event_type) = line
                .strip_prefix("event: ")
                .or_else(|| line.strip_prefix("event:"))
            {
                self.current_event = Some(event_type.trim().to_string());
            } else if let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                events.push(SseEvent {
                    event_type: self.current_event.clone(),
                    data: data.to_string(),
                });
            }
        }

        events
    }

    /// Flush any remaining data in the line buffer.
    pub fn flush(&mut self) -> Vec<SseEvent> {
        if self.line_buffer.is_empty() {
            return Vec::new();
        }

        let line = std::mem::take(&mut self.line_buffer);
        let line = line.trim_end_matches('\r');

        if let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            vec![SseEvent {
                event_type: self.current_event.clone(),
                data: data.to_string(),
            }]
        } else {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Usage extraction from SSE events
// ---------------------------------------------------------------------------

/// Token usage information extracted from an SSE stream.
#[derive(Debug, Clone, Default)]
pub struct StreamUsage {
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// Extract usage information from a parsed SSE event's data field.
///
/// Handles these event types:
/// - `message_start` — contains input_tokens, model, cache tokens
/// - `message_delta` — contains output_tokens (authoritative final count)
/// - `content_block_delta` — streaming text (not used for counting here)
pub fn extract_usage_from_sse_data(data: &str, usage: &mut StreamUsage) {
    let json: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return,
    };

    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "message_start" => {
            if let Some(message) = json.get("message") {
                if let Some(model) = message.get("model").and_then(|m| m.as_str()) {
                    usage.model = Some(model.to_string());
                }
                if let Some(u) = message.get("usage") {
                    if let Some(v) = u.get("input_tokens").and_then(|v| v.as_i64()) {
                        usage.input_tokens = Some(v);
                    }
                    if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_i64()) {
                        usage.cache_read_input_tokens = Some(v);
                    }
                    if let Some(v) = u
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_i64())
                    {
                        usage.cache_creation_input_tokens = Some(v);
                    }
                }
            }
        }
        "message_delta" => {
            if let Some(u) = json.get("usage") {
                if let Some(v) = u.get("output_tokens").and_then(|v| v.as_i64()) {
                    usage.output_tokens = Some(v);
                }
            }
        }
        _ => {}
    }
}

/// Extract usage information from a complete (non-streaming) response body.
pub fn extract_usage_from_response(body: &[u8]) -> Option<StreamUsage> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let u = json.get("usage")?;

    Some(StreamUsage {
        model: json.get("model").and_then(|m| m.as_str()).map(String::from),
        input_tokens: u.get("input_tokens").and_then(|v| v.as_i64()),
        output_tokens: u.get("output_tokens").and_then(|v| v.as_i64()),
        cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_i64()),
        cache_creation_input_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64()),
    })
}

/// Parse all SSE events from a complete analytics buffer and extract usage.
pub fn extract_usage_from_buffer(buffer: &AnalyticsBuffer) -> StreamUsage {
    let mut parser = SseParser::new();
    let mut usage = StreamUsage::default();

    for chunk in &buffer.chunks {
        let events = parser.feed(chunk);
        for event in events {
            extract_usage_from_sse_data(&event.data, &mut usage);
        }
    }

    // Flush any remaining data
    let events = parser.flush();
    for event in events {
        extract_usage_from_sse_data(&event.data, &mut usage);
    }

    usage
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- AnalyticsBuffer tests --

    #[test]
    fn buffer_accumulates_chunks() {
        let mut buf = AnalyticsBuffer::new();
        buf.push(&Bytes::from_static(b"hello"));
        buf.push(&Bytes::from_static(b"world"));
        assert_eq!(buf.total_bytes, 10);
        assert_eq!(buf.chunks.len(), 2);
        assert!(!buf.truncated);
    }

    #[test]
    fn buffer_truncates_at_limit() {
        let mut buf = AnalyticsBuffer::new();
        // Push a chunk that exceeds limit
        let big = Bytes::from(vec![b'x'; ANALYTICS_MAX_BYTES + 100]);
        buf.push(&big);
        assert_eq!(buf.total_bytes, ANALYTICS_MAX_BYTES);
        assert!(buf.truncated);
        assert_eq!(buf.chunks.len(), 1);

        // Further pushes are ignored
        buf.push(&Bytes::from_static(b"more"));
        assert_eq!(buf.total_bytes, ANALYTICS_MAX_BYTES);
        assert_eq!(buf.chunks.len(), 1);
    }

    #[test]
    fn buffer_partial_chunk_on_overflow() {
        let mut buf = AnalyticsBuffer::new();
        // Fill to near limit
        let first = Bytes::from(vec![b'a'; ANALYTICS_MAX_BYTES - 10]);
        buf.push(&first);
        assert_eq!(buf.total_bytes, ANALYTICS_MAX_BYTES - 10);

        // Push chunk that would overflow — only 10 bytes buffered
        let second = Bytes::from(vec![b'b'; 50]);
        buf.push(&second);
        assert_eq!(buf.total_bytes, ANALYTICS_MAX_BYTES);
        assert!(buf.truncated);
        assert_eq!(buf.chunks.len(), 2);
        assert_eq!(buf.chunks[1].len(), 10);
    }

    // -- SSE Parser tests --

    #[test]
    fn parse_simple_sse_event() {
        let mut parser = SseParser::new();
        let chunk = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
        let events = parser.feed(chunk);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, Some("message_start".to_string()));
        assert_eq!(events[0].data, "{\"type\":\"message_start\"}");
    }

    #[test]
    fn parse_multiple_events_in_one_chunk() {
        let mut parser = SseParser::new();
        let chunk = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\"}\n\n";
        let events = parser.feed(chunk);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, Some("message_start".to_string()));
        assert_eq!(events[1].event_type, Some("message_delta".to_string()));
    }

    #[test]
    fn parse_split_across_chunks() {
        let mut parser = SseParser::new();

        // First chunk ends mid-line
        let events1 = parser.feed(b"event: message_start\ndata: {\"ty");
        assert_eq!(events1.len(), 0); // data line incomplete

        // Second chunk completes the event
        let events2 = parser.feed(b"pe\":\"message_start\"}\n\n");
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].data, "{\"type\":\"message_start\"}");
    }

    #[test]
    fn parse_data_without_event_type() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: {\"type\":\"ping\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, None);
    }

    #[test]
    fn flush_incomplete_data() {
        let mut parser = SseParser::new();
        // No trailing newline
        let events = parser.feed(b"data: {\"type\":\"final\"}");
        assert_eq!(events.len(), 0);

        let events = parser.flush();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"type\":\"final\"}");
    }

    // -- Usage extraction tests --

    #[test]
    fn extract_message_start_usage() {
        let mut usage = StreamUsage::default();
        let data = r#"{"type":"message_start","message":{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":100,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#;
        extract_usage_from_sse_data(data, &mut usage);

        assert_eq!(usage.model, Some("claude-sonnet-4-5-20250929".to_string()));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.cache_read_input_tokens, Some(10));
        assert_eq!(usage.cache_creation_input_tokens, Some(5));
    }

    #[test]
    fn extract_message_delta_usage() {
        let mut usage = StreamUsage::default();
        let data = r#"{"type":"message_delta","usage":{"output_tokens":50}}"#;
        extract_usage_from_sse_data(data, &mut usage);

        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn extract_usage_ignores_content_block_delta() {
        let mut usage = StreamUsage::default();
        let data = r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}"#;
        extract_usage_from_sse_data(data, &mut usage);

        assert!(usage.model.is_none());
        assert!(usage.output_tokens.is_none());
    }

    #[test]
    fn extract_usage_from_non_streaming() {
        let body = br#"{
            "model": "claude-3-opus",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 100,
                "cache_read_input_tokens": 20
            }
        }"#;
        let usage = extract_usage_from_response(body).unwrap();
        assert_eq!(usage.model, Some("claude-3-opus".to_string()));
        assert_eq!(usage.input_tokens, Some(200));
        assert_eq!(usage.output_tokens, Some(100));
        assert_eq!(usage.cache_read_input_tokens, Some(20));
        assert!(usage.cache_creation_input_tokens.is_none());
    }

    #[test]
    fn extract_usage_from_analytics_buffer() {
        let mut buffer = AnalyticsBuffer::new();
        buffer.push(&Bytes::from(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-3-opus\",\"usage\":{\"input_tokens\":100}}}\n\n",
        ));
        buffer.push(&Bytes::from(
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":50}}\n\n",
        ));

        let usage = extract_usage_from_buffer(&buffer);
        assert_eq!(usage.model, Some("claude-3-opus".to_string()));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[test]
    fn extract_usage_invalid_json() {
        let mut usage = StreamUsage::default();
        extract_usage_from_sse_data("not json", &mut usage);
        assert!(usage.model.is_none());
    }

    // -- Tee stream tests --

    #[tokio::test]
    async fn tee_stream_buffers_for_analytics() {
        use futures::StreamExt;

        let chunks: Vec<Result<Bytes, Infallible>> = vec![
            Ok(Bytes::from_static(b"chunk1")),
            Ok(Bytes::from_static(b"chunk2")),
            Ok(Bytes::from_static(b"chunk3")),
        ];
        let inner = futures::stream::iter(chunks);
        let (tee, rx) = tee_stream(inner);

        // Consume the tee'd stream
        let collected: Vec<Bytes> = tee.map(|r| r.unwrap()).collect().await;
        assert_eq!(collected.len(), 3);
        assert_eq!(&collected[0][..], b"chunk1");

        // Analytics buffer received
        let buffer = rx.await.unwrap();
        assert_eq!(buffer.chunks.len(), 3);
        assert_eq!(buffer.total_bytes, 18); // 6 + 6 + 6
        assert!(!buffer.truncated);
    }

    #[tokio::test]
    async fn tee_stream_truncates_analytics() {
        use futures::StreamExt;

        let big_chunk = Bytes::from(vec![b'x'; ANALYTICS_MAX_BYTES + 100]);
        let chunks: Vec<Result<Bytes, Infallible>> = vec![Ok(big_chunk.clone())];
        let inner = futures::stream::iter(chunks);
        let (tee, rx) = tee_stream(inner);

        // Client gets full data
        let collected: Vec<Bytes> = tee.map(|r| r.unwrap()).collect().await;
        assert_eq!(collected[0].len(), ANALYTICS_MAX_BYTES + 100);

        // Analytics buffer is truncated
        let buffer = rx.await.unwrap();
        assert_eq!(buffer.total_bytes, ANALYTICS_MAX_BYTES);
        assert!(buffer.truncated);
    }
}
