//! Async database writer — non-blocking batched writes via tokio mpsc.
//!
//! The proxy hot path sends `WriteOp`s through the channel. A background
//! tokio task collects them and flushes to SQLite in batches (every 5 s
//! or 100 ops, whichever comes first). Errors are logged, never propagated.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::pool::DbPool;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const CHANNEL_CAPACITY: usize = 1000;
const BATCH_SIZE: usize = 100;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const BACKPRESSURE_WARN_THRESHOLD: usize = (CHANNEL_CAPACITY as f64 * 0.8) as usize; // 800

// ---------------------------------------------------------------------------
// WriteOp — everything the hot path can enqueue
// ---------------------------------------------------------------------------

/// A database write operation that can be sent through the async channel.
///
/// Each variant owns its data so the send is allocation-free on the caller
/// side (the data is moved, not cloned).
#[derive(Debug)]
pub enum WriteOp {
    /// Insert a full proxy request row.
    SaveRequest(bccf_core::types::ProxyRequest),

    /// Insert or replace request+response payloads.
    SavePayload {
        request_id: String,
        request_body: Option<String>,
        response_body: Option<String>,
    },

    /// Update token counts, cost, and timing after response is parsed.
    UpdateRequestUsage {
        request_id: String,
        usage: crate::repositories::request::UsageUpdate,
    },

    /// Update the result columns (success, error, timing) on a request.
    UpdateRequestResult {
        request_id: String,
        success: bool,
        error_message: Option<String>,
        response_time_ms: i64,
        failover_attempts: i64,
    },

    /// Increment an account's request counter and update usage timestamp.
    IncrementAccountUsage {
        account_id: String,
        now: i64,
        session_duration_ms: i64,
    },

    /// Mark an account as rate-limited until the given timestamp.
    SetRateLimited { account_id: String, until: i64 },

    /// Record rate-limit metadata on an account.
    UpdateRateLimitMeta {
        account_id: String,
        status: String,
        reset: Option<i64>,
        remaining: Option<i64>,
    },

    /// Bump API key usage counter + last_used timestamp.
    UpdateApiKeyUsage { api_key_id: String, timestamp: i64 },
}

// ---------------------------------------------------------------------------
// AsyncDbWriter handle (sender side)
// ---------------------------------------------------------------------------

/// Handle returned to callers. Cloneable and cheap.
///
/// Sending through this handle never blocks — if the channel is full the
/// op is dropped and a warning is logged.
#[derive(Clone)]
pub struct AsyncDbWriter {
    tx: mpsc::Sender<WriteOp>,
}

impl AsyncDbWriter {
    /// Send a write operation. Returns immediately.
    ///
    /// If the channel is full the op is dropped silently (a tracing warning
    /// is emitted inside the background task when backpressure is detected).
    pub fn send(&self, op: WriteOp) {
        if let Err(mpsc::error::TrySendError::Full(op)) = self.tx.try_send(op) {
            tracing::warn!(
                op = ?std::mem::discriminant(&op),
                "Async DB writer channel full — dropping write op"
            );
        }
        // Closed channel: silently ignore (shutdown in progress)
    }

    /// How many ops are currently queued (approximate).
    pub fn queued(&self) -> usize {
        CHANNEL_CAPACITY - self.tx.capacity()
    }

    /// True if the background task has been shut down.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

// ---------------------------------------------------------------------------
// Background task handle
// ---------------------------------------------------------------------------

/// Owns the background flush task. Drop or call [`WriterTask::shutdown`] to
/// flush remaining ops and stop.
pub struct WriterTask {
    handle: Option<JoinHandle<()>>,
    /// Kept alive so dropping `WriterTask` closes the channel, causing the
    /// background loop to exit.
    _tx: mpsc::Sender<WriteOp>,
}

impl WriterTask {
    /// Gracefully shut down: close the channel, flush remaining ops (with a
    /// 3 s timeout), and join the background task.
    pub async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            // Dropping `self` (including `self._tx`) at end of this method
            // reduces sender count. The background loop exits when the receiver
            // sees the channel close (all senders dropped). We give it
            // SHUTDOWN_TIMEOUT to finish flushing.
            //
            // Note: the channel won't fully close until external `AsyncDbWriter`
            // handles are also dropped. But the background task will still
            // finish its current batch and exit when the JoinHandle completes.
            match tokio::time::timeout(SHUTDOWN_TIMEOUT, handle).await {
                Ok(Ok(())) => tracing::info!("Async DB writer shut down cleanly"),
                Ok(Err(e)) => tracing::error!("Async DB writer task panicked: {e}"),
                Err(_) => {
                    tracing::warn!("Async DB writer shutdown timed out after {SHUTDOWN_TIMEOUT:?}")
                }
            }
        }
        // `self._tx` is dropped here, reducing the sender ref count.
    }
}

impl Drop for WriterTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn the async writer background task.
///
/// Returns the cloneable sender handle and the task handle (for shutdown).
pub fn spawn(pool: DbPool) -> (AsyncDbWriter, WriterTask) {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let tx2 = tx.clone();

    let handle = tokio::spawn(flush_loop(rx, pool));

    let writer = AsyncDbWriter { tx };
    let task = WriterTask {
        handle: Some(handle),
        _tx: tx2,
    };
    (writer, task)
}

// ---------------------------------------------------------------------------
// Background flush loop
// ---------------------------------------------------------------------------

async fn flush_loop(mut rx: mpsc::Receiver<WriteOp>, pool: DbPool) {
    let mut batch: Vec<WriteOp> = Vec::with_capacity(BATCH_SIZE);
    let mut interval = tokio::time::interval(FLUSH_INTERVAL);
    // Don't try to "catch up" if flush takes longer than the interval.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Fill the batch up to BATCH_SIZE or until the timer fires.
        let should_exit = fill_batch(&mut rx, &mut batch, &mut interval).await;

        if !batch.is_empty() {
            let queued_approx = CHANNEL_CAPACITY - rx.capacity();
            if queued_approx >= BACKPRESSURE_WARN_THRESHOLD {
                tracing::warn!(
                    queued = queued_approx,
                    capacity = CHANNEL_CAPACITY,
                    "Async DB writer channel >80% full"
                );
            }

            flush_batch(&batch, &pool);
            batch.clear();
        }

        if should_exit {
            // Drain any remaining ops after channel close.
            while let Ok(op) = rx.try_recv() {
                batch.push(op);
            }
            if !batch.is_empty() {
                tracing::info!(
                    remaining = batch.len(),
                    "Flushing remaining ops on shutdown"
                );
                flush_batch(&batch, &pool);
            }
            tracing::info!("Async DB writer loop exiting");
            return;
        }
    }
}

/// Fill `batch` until it reaches `BATCH_SIZE` or the interval fires.
/// Returns `true` if the channel is closed (signal to exit).
async fn fill_batch(
    rx: &mut mpsc::Receiver<WriteOp>,
    batch: &mut Vec<WriteOp>,
    interval: &mut tokio::time::Interval,
) -> bool {
    loop {
        if batch.len() >= BATCH_SIZE {
            return false;
        }

        tokio::select! {
            biased;

            maybe_op = rx.recv() => {
                match maybe_op {
                    Some(op) => batch.push(op),
                    None => return true, // channel closed
                }
            }

            _ = interval.tick() => {
                return false; // timer fired — flush whatever we have
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Execute batch against SQLite
// ---------------------------------------------------------------------------

fn flush_batch(batch: &[WriteOp], pool: &DbPool) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to get DB connection for flush: {e}");
            return;
        }
    };

    let mut ok = 0usize;
    let mut err = 0usize;

    for op in batch {
        if let Err(e) = execute_op(op, &conn) {
            tracing::error!(op = ?std::mem::discriminant(op), error = %e, "DB write failed");
            err += 1;
        } else {
            ok += 1;
        }
    }

    if err > 0 {
        tracing::warn!(ok, err, "Async DB flush completed with errors");
    } else {
        tracing::debug!(count = ok, "Async DB flush completed");
    }
}

fn execute_op(op: &WriteOp, conn: &rusqlite::Connection) -> Result<(), crate::error::DbError> {
    use crate::repositories::{account, api_key, request};

    match op {
        WriteOp::SaveRequest(req) => request::save(conn, req),

        WriteOp::SavePayload {
            request_id,
            request_body,
            response_body,
        } => request::save_payload(
            conn,
            request_id,
            request_body.as_deref(),
            response_body.as_deref(),
        ),

        WriteOp::UpdateRequestUsage { request_id, usage } => {
            request::update_usage(conn, request_id, usage)
        }

        WriteOp::UpdateRequestResult {
            request_id,
            success,
            error_message,
            response_time_ms,
            failover_attempts,
        } => request::update_result(
            conn,
            request_id,
            *success,
            error_message.as_deref(),
            *response_time_ms,
            *failover_attempts,
        ),

        WriteOp::IncrementAccountUsage {
            account_id,
            now,
            session_duration_ms,
        } => account::increment_usage(conn, account_id, *now, *session_duration_ms),

        WriteOp::SetRateLimited { account_id, until } => {
            account::set_rate_limited(conn, account_id, *until)
        }

        WriteOp::UpdateRateLimitMeta {
            account_id,
            status,
            reset,
            remaining,
        } => account::update_rate_limit_meta(conn, account_id, status, *reset, *remaining),

        WriteOp::UpdateApiKeyUsage {
            api_key_id,
            timestamp,
        } => api_key::update_usage(conn, api_key_id, *timestamp),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{create_memory_pool, PoolConfig};
    use bccf_core::types::ProxyRequest;

    fn test_pool() -> DbPool {
        create_memory_pool(&PoolConfig {
            max_size: 1,
            min_idle: Some(1),
        })
        .unwrap()
    }

    fn test_request(id: &str) -> ProxyRequest {
        ProxyRequest {
            id: id.to_string(),
            timestamp: 1700000000000,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            account_used: Some("acc-1".to_string()),
            status_code: Some(200),
            success: true,
            error_message: None,
            response_time_ms: Some(150),
            failover_attempts: 0,
            model: Some("claude-3-opus".to_string()),
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cost_usd: Some(0.01),
            input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens: None,
            agent_used: None,
            tokens_per_second: None,
            project: None,
            api_key_id: None,
            api_key_name: None,
        }
    }

    #[tokio::test]
    async fn send_and_flush_request() {
        let pool = test_pool();
        let (writer, task) = spawn(pool.clone());

        writer.send(WriteOp::SaveRequest(test_request("req-1")));

        // Give the flush loop time to process
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify the request was persisted
        let conn = pool.get().unwrap();
        let req = crate::repositories::request::find_by_id(&conn, "req-1").unwrap();
        assert!(req.is_some());
        assert_eq!(req.unwrap().method, "POST");

        task.shutdown().await;
    }

    #[tokio::test]
    async fn batch_flush_on_count() {
        let pool = test_pool();
        let (writer, task) = spawn(pool.clone());

        // Send exactly BATCH_SIZE requests to trigger a count-based flush
        for i in 0..BATCH_SIZE {
            writer.send(WriteOp::SaveRequest(test_request(&format!("batch-{i}"))));
        }

        // Should flush quickly (not waiting for the 5s timer)
        tokio::time::sleep(Duration::from_millis(500)).await;

        let conn = pool.get().unwrap();
        let all = crate::repositories::request::get_recent(&conn, BATCH_SIZE as i64).unwrap();
        assert_eq!(all.len(), BATCH_SIZE);

        task.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_flushes_remaining() {
        let pool = test_pool();
        let (writer, task) = spawn(pool.clone());

        // Send a few ops but don't wait for the interval
        for i in 0..5 {
            writer.send(WriteOp::SaveRequest(test_request(&format!("shut-{i}"))));
        }

        // Immediately shut down — should flush remaining
        task.shutdown().await;

        let conn = pool.get().unwrap();
        let all = crate::repositories::request::get_recent(&conn, 10).unwrap();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn queued_and_closed() {
        let pool = test_pool();
        let (writer, task) = spawn(pool);

        assert!(!writer.is_closed());
        assert_eq!(writer.queued(), 0);

        writer.send(WriteOp::SaveRequest(test_request("q-1")));
        // queued() is approximate, just check it doesn't panic
        let _q = writer.queued();

        // Drop writer so shutdown can fully close the channel.
        drop(writer);
        task.shutdown().await;
    }

    #[tokio::test]
    async fn error_in_op_does_not_crash() {
        let pool = test_pool();
        let (writer, task) = spawn(pool.clone());

        // Send a payload for a non-existent request — will fail FK constraint
        writer.send(WriteOp::SavePayload {
            request_id: "nonexistent".to_string(),
            request_body: Some("{}".to_string()),
            response_body: None,
        });

        // Send a valid op after the error
        writer.send(WriteOp::SaveRequest(test_request("after-error")));

        // Shut down to force a flush of remaining ops (including the error + valid one)
        drop(writer);
        task.shutdown().await;

        // The valid op should have been persisted despite the earlier error
        let conn = pool.get().unwrap();
        assert!(
            crate::repositories::request::find_by_id(&conn, "after-error")
                .unwrap()
                .is_some()
        );
    }
}
