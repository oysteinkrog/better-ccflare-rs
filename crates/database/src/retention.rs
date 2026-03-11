//! Data retention service — automatic cleanup of old request history and payloads.
//!
//! Runs on startup and then daily via a `tokio::time::interval`. Deletes in
//! batches of 1000 rows to avoid holding long database locks.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{params, Connection};
use tracing::{debug, info, warn};

use crate::pool::DbPool;

/// Maximum rows deleted per batch to avoid long DB locks.
const BATCH_SIZE: i64 = 1000;

/// Interval between cleanup runs (24 hours).
const CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Milliseconds per day.
const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// Configuration for the retention service.
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// Days to keep request payloads (default 7).
    pub data_retention_days: u32,
    /// Days to keep request metadata (default 365).
    pub request_retention_days: u32,
    /// Days to keep xfactor observations (default 30).
    pub xfactor_retention_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            data_retention_days: 7,
            request_retention_days: 365,
            xfactor_retention_days: 30,
        }
    }
}

/// Handle to stop the retention service.
pub struct RetentionTask {
    handle: tokio::task::JoinHandle<()>,
}

impl RetentionTask {
    /// Stop the retention service and wait for it to finish.
    pub async fn shutdown(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

/// Spawn the retention cleanup service.
///
/// Runs cleanup immediately, then every 24 hours. Returns a handle that
/// can be used to stop the service on shutdown.
pub fn spawn(pool: DbPool, config: RetentionConfig) -> RetentionTask {
    let pool = Arc::new(pool);
    let handle = tokio::spawn(async move {
        // Run immediately on startup
        run_cleanup(&pool, &config);

        // Then run on interval
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.tick().await; // Consume the immediate tick
        loop {
            interval.tick().await;
            run_cleanup(&pool, &config);
        }
    });

    RetentionTask { handle }
}

/// Execute a single cleanup pass. Deletes payloads first, then requests,
/// then orphaned payloads.
fn run_cleanup(pool: &DbPool, config: &RetentionConfig) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            warn!("Retention cleanup: failed to get DB connection: {e}");
            return;
        }
    };

    let now_ms = chrono::Utc::now().timestamp_millis();

    // Clean payloads first (they reference requests via FK)
    let payload_cutoff = now_ms - (config.data_retention_days as i64) * MS_PER_DAY;
    let payloads_deleted = batched_delete_payloads(&conn, payload_cutoff);

    // Clean request metadata
    let request_cutoff = now_ms - (config.request_retention_days as i64) * MS_PER_DAY;
    let requests_deleted = batched_delete_requests(&conn, request_cutoff);

    // Clean orphaned payloads
    let orphans_deleted = delete_orphaned_payloads_batched(&conn);

    // Clean old xfactor observations
    let xfactor_cutoff = now_ms - (config.xfactor_retention_days as i64) * MS_PER_DAY;
    let xfactor_deleted = batched_delete_xfactor_observations(&conn, xfactor_cutoff);

    if payloads_deleted > 0 || requests_deleted > 0 || orphans_deleted > 0 || xfactor_deleted > 0 {
        info!(
            "Retention cleanup: deleted {payloads_deleted} payloads, \
             {requests_deleted} requests, {orphans_deleted} orphans, \
             {xfactor_deleted} xfactor observations"
        );
    } else {
        debug!("Retention cleanup: nothing to delete");
    }
}

/// Delete payloads older than cutoff in batches.
fn batched_delete_payloads(conn: &Connection, cutoff_ts: i64) -> usize {
    let mut total = 0;
    loop {
        match conn.execute(
            "DELETE FROM request_payloads WHERE request_id IN (
                SELECT request_id FROM request_payloads
                WHERE request_id IN (SELECT id FROM requests WHERE timestamp < ?1)
                LIMIT ?2
            )",
            params![cutoff_ts, BATCH_SIZE],
        ) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                debug!("Retention: deleted batch of {n} payloads (total so far: {total})");
            }
            Err(e) => {
                warn!("Retention: error deleting payloads: {e}");
                break;
            }
        }
    }
    total
}

/// Delete requests older than cutoff in batches.
fn batched_delete_requests(conn: &Connection, cutoff_ts: i64) -> usize {
    let mut total = 0;
    loop {
        match conn.execute(
            "DELETE FROM requests WHERE id IN (
                SELECT id FROM requests WHERE timestamp < ?1 LIMIT ?2
            )",
            params![cutoff_ts, BATCH_SIZE],
        ) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                debug!("Retention: deleted batch of {n} requests (total so far: {total})");
            }
            Err(e) => {
                warn!("Retention: error deleting requests: {e}");
                break;
            }
        }
    }
    total
}

/// Delete xfactor_observations older than cutoff in batches.
fn batched_delete_xfactor_observations(conn: &Connection, cutoff_ts: i64) -> usize {
    let mut total = 0;
    loop {
        match conn.execute(
            "DELETE FROM xfactor_observations WHERE id IN (
                SELECT id FROM xfactor_observations WHERE timestamp_ms < ?1 LIMIT ?2
            )",
            params![cutoff_ts, BATCH_SIZE],
        ) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                debug!("Retention: deleted batch of {n} xfactor observations (total so far: {total})");
            }
            Err(e) => {
                warn!("Retention: error deleting xfactor observations: {e}");
                break;
            }
        }
    }
    total
}

/// Delete orphaned payloads (no matching request) in batches.
fn delete_orphaned_payloads_batched(conn: &Connection) -> usize {
    let mut total = 0;
    loop {
        match conn.execute(
            "DELETE FROM request_payloads WHERE request_id IN (
                SELECT request_id FROM request_payloads
                WHERE request_id NOT IN (SELECT id FROM requests)
                LIMIT ?1
            )",
            params![BATCH_SIZE],
        ) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                debug!("Retention: deleted batch of {n} orphaned payloads (total so far: {total})");
            }
            Err(e) => {
                warn!("Retention: error deleting orphaned payloads: {e}");
                break;
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_tables(&conn).unwrap();
        conn
    }

    fn insert_request(conn: &Connection, id: &str, timestamp: i64) {
        conn.execute(
            "INSERT INTO requests (id, timestamp, method, path, success, failover_attempts)
             VALUES (?1, ?2, 'POST', '/v1/messages', 1, 0)",
            params![id, timestamp],
        )
        .unwrap();
    }

    fn insert_payload(conn: &Connection, request_id: &str) {
        conn.execute(
            "INSERT INTO request_payloads (request_id, request_body, response_body)
             VALUES (?1, '{}', '{}')",
            params![request_id],
        )
        .unwrap();
    }

    fn count_requests(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
            .unwrap()
    }

    fn count_payloads(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM request_payloads", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn batched_delete_requests_removes_old() {
        let conn = setup_test_db();
        // Insert 5 requests: 3 old, 2 new
        let cutoff = 10_000;
        for i in 0..3 {
            insert_request(&conn, &format!("old-{i}"), 1000 + i);
        }
        for i in 0..2 {
            insert_request(&conn, &format!("new-{i}"), 20_000 + i);
        }

        assert_eq!(count_requests(&conn), 5);
        let deleted = batched_delete_requests(&conn, cutoff);
        assert_eq!(deleted, 3);
        assert_eq!(count_requests(&conn), 2);
    }

    #[test]
    fn batched_delete_requests_nothing_to_delete() {
        let conn = setup_test_db();
        insert_request(&conn, "recent", 100_000);

        let deleted = batched_delete_requests(&conn, 1000);
        assert_eq!(deleted, 0);
        assert_eq!(count_requests(&conn), 1);
    }

    #[test]
    fn batched_delete_payloads_removes_old() {
        let conn = setup_test_db();
        let cutoff = 10_000;

        // Old request with payload
        insert_request(&conn, "old-1", 1000);
        insert_payload(&conn, "old-1");
        insert_request(&conn, "old-2", 2000);
        insert_payload(&conn, "old-2");

        // New request with payload
        insert_request(&conn, "new-1", 20_000);
        insert_payload(&conn, "new-1");

        assert_eq!(count_payloads(&conn), 3);
        let deleted = batched_delete_payloads(&conn, cutoff);
        assert_eq!(deleted, 2);
        assert_eq!(count_payloads(&conn), 1);
    }

    #[test]
    fn delete_orphaned_payloads_cleans_up() {
        let conn = setup_test_db();

        // Insert a request with payload
        insert_request(&conn, "valid", 100_000);
        insert_payload(&conn, "valid");

        // Create an orphan: insert request + payload, then delete the request
        insert_request(&conn, "orphan-src", 50_000);
        insert_payload(&conn, "orphan-src");
        // Temporarily disable FK to delete just the request
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute("DELETE FROM requests WHERE id = 'orphan-src'", [])
            .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();

        assert_eq!(count_payloads(&conn), 2);
        let deleted = delete_orphaned_payloads_batched(&conn);
        assert_eq!(deleted, 1);
        assert_eq!(count_payloads(&conn), 1);
    }

    #[test]
    fn run_cleanup_handles_empty_db() {
        let conn = setup_test_db();
        let config = RetentionConfig::default();
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Should not panic on empty DB
        let payload_cutoff = now_ms - (config.data_retention_days as i64) * MS_PER_DAY;
        let request_cutoff = now_ms - (config.request_retention_days as i64) * MS_PER_DAY;

        assert_eq!(batched_delete_payloads(&conn, payload_cutoff), 0);
        assert_eq!(batched_delete_requests(&conn, request_cutoff), 0);
        assert_eq!(delete_orphaned_payloads_batched(&conn), 0);
    }

    #[test]
    fn retention_config_defaults() {
        let config = RetentionConfig::default();
        assert_eq!(config.data_retention_days, 7);
        assert_eq!(config.request_retention_days, 365);
    }

    #[test]
    fn batched_delete_respects_cutoff_exactly() {
        let conn = setup_test_db();
        // Request at exactly the cutoff timestamp should NOT be deleted
        // (DELETE WHERE timestamp < cutoff)
        insert_request(&conn, "at-cutoff", 10_000);
        insert_request(&conn, "before-cutoff", 9_999);
        insert_request(&conn, "after-cutoff", 10_001);

        let deleted = batched_delete_requests(&conn, 10_000);
        assert_eq!(deleted, 1); // Only before-cutoff
        assert_eq!(count_requests(&conn), 2);
    }

    #[test]
    fn full_cleanup_flow() {
        let conn = setup_test_db();
        let now_ms = 100_000_000i64;

        // Old data (older than 7 days)
        let old_ts = now_ms - 8 * MS_PER_DAY;
        insert_request(&conn, "old-req", old_ts);
        insert_payload(&conn, "old-req");

        // Recent data (within 7 days)
        let recent_ts = now_ms - 1 * MS_PER_DAY;
        insert_request(&conn, "new-req", recent_ts);
        insert_payload(&conn, "new-req");

        // Cleanup with 7-day payload retention
        let payload_cutoff = now_ms - 7 * MS_PER_DAY;
        let payloads_deleted = batched_delete_payloads(&conn, payload_cutoff);
        assert_eq!(payloads_deleted, 1);

        // Request metadata with 365-day retention — nothing should be deleted
        let request_cutoff = now_ms - 365 * MS_PER_DAY;
        let requests_deleted = batched_delete_requests(&conn, request_cutoff);
        assert_eq!(requests_deleted, 0);

        // Final state
        assert_eq!(count_requests(&conn), 2); // Both requests remain
        assert_eq!(count_payloads(&conn), 1); // Only recent payload
    }
}
