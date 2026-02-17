//! Post-processor — background analytics extraction from streaming responses.
//!
//! Receives analytics buffers from the stream tee, parses SSE events, extracts
//! token usage, computes cost, and sends summaries to the async DB writer.
//!
//! Limits:
//! - 512 KB chunk buffer per request
//! - 10,000 max concurrent request entries
//! - 5-minute TTL per request
//! - 30-second cleanup interval

use std::time::Instant;

use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::pricing::{self, TokenBreakdown};
use crate::streaming::{self, AnalyticsBuffer, StreamUsage};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum bytes accumulated in the chunk buffer per request (512 KB).
pub const MAX_CHUNKS_BYTES: usize = 512 * 1024;

/// Maximum number of tracked requests before emergency cleanup.
pub const MAX_REQUESTS_MAP_SIZE: usize = 10_000;

/// Time-to-live for a request entry (5 minutes).
pub const REQUEST_TTL_MS: u64 = 5 * 60 * 1000;

/// Cleanup interval for stale requests (30 seconds).
pub const CLEANUP_INTERVAL_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Messages sent to the post-processor task.
#[derive(Debug)]
pub enum PostProcessorMsg {
    /// A streaming response has completed. Process the analytics buffer.
    StreamComplete {
        request_id: String,
        account_id: Option<String>,
        path: String,
        buffer: AnalyticsBuffer,
        response_status: u16,
        start_time: Instant,
        agent_used: Option<String>,
        project: Option<String>,
        api_key_id: Option<String>,
        api_key_name: Option<String>,
        failover_attempts: usize,
    },
    /// A non-streaming response has completed. Process the response body.
    ResponseComplete {
        request_id: String,
        account_id: Option<String>,
        path: String,
        body: Bytes,
        response_status: u16,
        start_time: Instant,
        agent_used: Option<String>,
        project: Option<String>,
        api_key_id: Option<String>,
        api_key_name: Option<String>,
        failover_attempts: usize,
    },
    /// Shutdown the post-processor.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Analytics summary
// ---------------------------------------------------------------------------

/// Final analytics summary for a completed request.
#[derive(Debug, Clone)]
pub struct RequestSummary {
    pub request_id: String,
    pub account_id: Option<String>,
    pub path: String,
    pub status_code: u16,
    pub success: bool,
    pub response_time_ms: u64,
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub agent_used: Option<String>,
    pub project: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub failover_attempts: usize,
    pub tokens_per_second: Option<f64>,
}

/// Build a request summary from usage data.
#[allow(clippy::too_many_arguments)]
fn build_summary(
    request_id: String,
    account_id: Option<String>,
    path: String,
    status_code: u16,
    start_time: Instant,
    usage: StreamUsage,
    agent_used: Option<String>,
    project: Option<String>,
    api_key_id: Option<String>,
    api_key_name: Option<String>,
    failover_attempts: usize,
) -> RequestSummary {
    let response_time_ms = start_time.elapsed().as_millis() as u64;
    let success = (200..300).contains(&status_code);

    let tokens_breakdown: TokenBreakdown = (&usage).into();
    let prompt_tokens = tokens_breakdown.input_tokens
        + tokens_breakdown.cache_read_input_tokens
        + tokens_breakdown.cache_creation_input_tokens;
    let total_tokens = prompt_tokens + tokens_breakdown.output_tokens;

    let cost_usd = usage
        .model
        .as_deref()
        .map(|model| pricing::estimate_cost_usd(model, &tokens_breakdown));

    // Tokens per second: output_tokens / response_time
    let tokens_per_second = if response_time_ms > 0 {
        usage
            .output_tokens
            .map(|out| out as f64 / (response_time_ms as f64 / 1000.0))
    } else {
        None
    };

    RequestSummary {
        request_id,
        account_id,
        path,
        status_code,
        success,
        response_time_ms,
        model: usage.model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        total_tokens: if total_tokens > 0 {
            Some(total_tokens)
        } else {
            None
        },
        cost_usd,
        agent_used,
        project,
        api_key_id,
        api_key_name,
        failover_attempts,
        tokens_per_second,
    }
}

// ---------------------------------------------------------------------------
// Post-processor task
// ---------------------------------------------------------------------------

/// Handle for sending messages to the post-processor.
#[derive(Clone)]
pub struct PostProcessorHandle {
    tx: mpsc::Sender<PostProcessorMsg>,
}

impl PostProcessorHandle {
    /// Send a message to the post-processor. Non-blocking, drops on full queue.
    pub fn send(&self, msg: PostProcessorMsg) {
        if self.tx.try_send(msg).is_err() {
            warn!("Post-processor queue full, dropping analytics message");
        }
    }

    /// Send a shutdown signal.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(PostProcessorMsg::Shutdown).await;
    }
}

/// Callback trait for receiving completed request summaries.
pub trait SummaryReceiver: Send + Sync + 'static {
    fn on_summary(&self, summary: RequestSummary);
}

/// Spawn the post-processor background task.
///
/// Returns a handle for sending messages. The task runs until Shutdown is
/// received or all senders are dropped.
pub fn spawn_post_processor<R: SummaryReceiver>(receiver: R) -> PostProcessorHandle {
    let (tx, mut rx) = mpsc::channel::<PostProcessorMsg>(1024);
    let receiver = std::sync::Arc::new(receiver);

    tokio::spawn(async move {
        info!("Post-processor started");

        while let Some(msg) = rx.recv().await {
            match msg {
                PostProcessorMsg::StreamComplete {
                    request_id,
                    account_id,
                    path,
                    buffer,
                    response_status,
                    start_time,
                    agent_used,
                    project,
                    api_key_id,
                    api_key_name,
                    failover_attempts,
                } => {
                    debug!(request_id = %request_id, "Processing stream analytics");
                    let usage = streaming::extract_usage_from_buffer(&buffer);
                    let summary = build_summary(
                        request_id,
                        account_id,
                        path,
                        response_status,
                        start_time,
                        usage,
                        agent_used,
                        project,
                        api_key_id,
                        api_key_name,
                        failover_attempts,
                    );
                    // Offload DB write to blocking thread to avoid blocking the async runtime
                    let recv = receiver.clone();
                    tokio::task::spawn_blocking(move || recv.on_summary(summary));
                }
                PostProcessorMsg::ResponseComplete {
                    request_id,
                    account_id,
                    path,
                    body,
                    response_status,
                    start_time,
                    agent_used,
                    project,
                    api_key_id,
                    api_key_name,
                    failover_attempts,
                } => {
                    debug!(request_id = %request_id, "Processing response analytics");
                    let usage = streaming::extract_usage_from_response(&body).unwrap_or_default();
                    let summary = build_summary(
                        request_id,
                        account_id,
                        path,
                        response_status,
                        start_time,
                        usage,
                        agent_used,
                        project,
                        api_key_id,
                        api_key_name,
                        failover_attempts,
                    );
                    let recv = receiver.clone();
                    tokio::task::spawn_blocking(move || recv.on_summary(summary));
                }
                PostProcessorMsg::Shutdown => {
                    info!("Post-processor shutting down");
                    break;
                }
            }
        }

        info!("Post-processor stopped");
    });

    PostProcessorHandle { tx }
}

// ---------------------------------------------------------------------------
// DB-backed summary receiver
// ---------------------------------------------------------------------------

/// Writes `RequestSummary` records to the `requests` table via a connection pool.
pub struct DbSummaryReceiver {
    pool: bccf_database::pool::DbPool,
}

impl DbSummaryReceiver {
    pub fn new(pool: bccf_database::pool::DbPool) -> Self {
        Self { pool }
    }
}

impl SummaryReceiver for DbSummaryReceiver {
    fn on_summary(&self, summary: RequestSummary) {
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "Failed to get DB connection for request recording");
                return;
            }
        };

        let req = bccf_core::types::ProxyRequest {
            id: summary.request_id.clone(),
            timestamp: chrono::Utc::now().timestamp_millis(),
            method: "POST".to_string(),
            path: summary.path.clone(),
            account_used: summary.account_id.clone(),
            status_code: Some(summary.status_code as i64),
            success: summary.success,
            error_message: None,
            response_time_ms: Some(summary.response_time_ms as i64),
            failover_attempts: summary.failover_attempts as i64,
            model: summary.model.clone(),
            prompt_tokens: summary.input_tokens,
            completion_tokens: summary.output_tokens,
            total_tokens: summary.total_tokens,
            cost_usd: summary.cost_usd,
            input_tokens: summary.input_tokens,
            cache_read_input_tokens: summary.cache_read_input_tokens,
            cache_creation_input_tokens: summary.cache_creation_input_tokens,
            output_tokens: summary.output_tokens,
            agent_used: summary.agent_used.clone(),
            tokens_per_second: summary.tokens_per_second,
            project: summary.project.clone(),
            api_key_id: summary.api_key_id.clone(),
            api_key_name: summary.api_key_name.clone(),
        };

        if let Err(e) = bccf_database::repositories::request::save(&conn, &req) {
            warn!(request_id = %summary.request_id, error = %e, "Failed to save request to DB");
        } else {
            debug!(request_id = %summary.request_id, model = ?summary.model, success = summary.success, "Recorded request to DB");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Test receiver that collects summaries.
    struct CollectingSummaryReceiver {
        summaries: Arc<Mutex<Vec<RequestSummary>>>,
    }

    impl SummaryReceiver for CollectingSummaryReceiver {
        fn on_summary(&self, summary: RequestSummary) {
            self.summaries.lock().unwrap().push(summary);
        }
    }

    #[test]
    fn build_summary_basic() {
        let usage = StreamUsage {
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_input_tokens: Some(10),
            cache_creation_input_tokens: Some(5),
        };
        let summary = build_summary(
            "req-1".to_string(),
            Some("acc-1".to_string()),
            "/v1/messages".to_string(),
            200,
            Instant::now(),
            usage,
            Some("claude-code".to_string()),
            None,
            None,
            None,
            0,
        );

        assert_eq!(summary.request_id, "req-1");
        assert!(summary.success);
        assert_eq!(
            summary.model,
            Some("claude-sonnet-4-5-20250929".to_string())
        );
        assert_eq!(summary.input_tokens, Some(100));
        assert_eq!(summary.output_tokens, Some(50));
        // total = input(100) + cache_read(10) + cache_creation(5) + output(50) = 165
        assert_eq!(summary.total_tokens, Some(165));
        assert!(summary.cost_usd.is_some());
        assert!(summary.cost_usd.unwrap() > 0.0);
    }

    #[test]
    fn build_summary_error_response() {
        let usage = StreamUsage::default();
        let summary = build_summary(
            "req-2".to_string(),
            None,
            "/v1/messages".to_string(),
            500,
            Instant::now(),
            usage,
            None,
            None,
            None,
            None,
            2,
        );

        assert!(!summary.success);
        assert_eq!(summary.failover_attempts, 2);
        assert!(summary.total_tokens.is_none()); // 0 total → None
    }

    #[tokio::test]
    async fn post_processor_handles_stream_complete() {
        let summaries = Arc::new(Mutex::new(Vec::new()));
        let receiver = CollectingSummaryReceiver {
            summaries: summaries.clone(),
        };

        let handle = spawn_post_processor(receiver);

        // Build a buffer with SSE data
        let mut buffer = AnalyticsBuffer {
            chunks: vec![],
            total_bytes: 0,
            truncated: false,
        };
        buffer.chunks.push(Bytes::from(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-3-opus\",\"usage\":{\"input_tokens\":100}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":50}}\n\n"
        ));
        buffer.total_bytes = buffer.chunks[0].len();

        handle.send(PostProcessorMsg::StreamComplete {
            request_id: "req-test".to_string(),
            account_id: Some("acc-1".to_string()),
            path: "/v1/messages".to_string(),
            buffer,
            response_status: 200,
            start_time: Instant::now(),
            agent_used: None,
            project: None,
            api_key_id: None,
            api_key_name: None,
            failover_attempts: 0,
        });

        // Give time for processing
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let results = summaries.lock().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model, Some("claude-3-opus".to_string()));
        assert_eq!(results[0].input_tokens, Some(100));
        assert_eq!(results[0].output_tokens, Some(50));
    }

    #[tokio::test]
    async fn post_processor_handles_non_streaming() {
        let summaries = Arc::new(Mutex::new(Vec::new()));
        let receiver = CollectingSummaryReceiver {
            summaries: summaries.clone(),
        };

        let handle = spawn_post_processor(receiver);

        let body = Bytes::from(
            r#"{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":200,"output_tokens":100}}"#,
        );

        handle.send(PostProcessorMsg::ResponseComplete {
            request_id: "req-non-stream".to_string(),
            account_id: None,
            path: "/v1/messages".to_string(),
            body,
            response_status: 200,
            start_time: Instant::now(),
            agent_used: None,
            project: None,
            api_key_id: None,
            api_key_name: None,
            failover_attempts: 0,
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        handle.shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let results = summaries.lock().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].model,
            Some("claude-sonnet-4-5-20250929".to_string())
        );
        assert_eq!(results[0].input_tokens, Some(200));
        assert_eq!(results[0].output_tokens, Some(100));
    }

    #[tokio::test]
    async fn post_processor_shutdown() {
        let summaries = Arc::new(Mutex::new(Vec::new()));
        let receiver = CollectingSummaryReceiver {
            summaries: summaries.clone(),
        };

        let handle = spawn_post_processor(receiver);
        handle.shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should not panic or hang
        assert!(summaries.lock().unwrap().is_empty());
    }
}
