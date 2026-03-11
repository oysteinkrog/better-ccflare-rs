//! Per-key request rate limiting (sliding window).
//!
//! `RpmTracker` tracks per-API-key request timestamps in a rolling 60-second
//! window. When the count exceeds the configured limit, `check_and_record`
//! returns `false` and the caller should respond with 429.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sliding-window per-key requests-per-minute tracker.
///
/// All operations are O(W/T) amortised where W is the window size and T is
/// the inter-request gap (stale entries are pruned on each call). The struct
/// is cheaply cloneable via `Arc`.
///
/// Thread-safe via a single `Mutex`. For typical proxy workloads with hundreds
/// of distinct API keys the lock contention is negligible.
pub struct RpmTracker {
    inner: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RpmTracker {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether `key_id` is within its rate limit and record the request.
    ///
    /// Returns `true` if the request is allowed, `false` if the per-minute
    /// limit has been reached. Uses a rolling 60-second window.
    pub fn check_and_record(&self, key_id: &str, rpm_limit: u32) -> bool {
        let now = Instant::now();
        let window = Duration::from_secs(60);

        let mut map = self.inner.lock().unwrap();
        let queue = map.entry(key_id.to_string()).or_default();

        // Remove timestamps that have fallen outside the window.
        while queue
            .front()
            .map(|t| now.duration_since(*t) > window)
            .unwrap_or(false)
        {
            queue.pop_front();
        }

        if queue.len() >= rpm_limit as usize {
            return false;
        }

        queue.push_back(now);
        true
    }
}

impl Default for RpmTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_within_limit() {
        let tracker = RpmTracker::new();
        for _ in 0..10 {
            assert!(tracker.check_and_record("key1", 10));
        }
    }

    #[test]
    fn blocks_at_limit() {
        let tracker = RpmTracker::new();
        for _ in 0..5 {
            assert!(tracker.check_and_record("key1", 5));
        }
        assert!(!tracker.check_and_record("key1", 5));
    }

    #[test]
    fn different_keys_are_independent() {
        let tracker = RpmTracker::new();
        for _ in 0..5 {
            assert!(tracker.check_and_record("key1", 5));
        }
        // key2 shares no quota with key1
        assert!(tracker.check_and_record("key2", 5));
    }

    #[test]
    fn zero_limit_always_blocks() {
        let tracker = RpmTracker::new();
        // rpm_limit = 0 → queue.len() (0) >= 0 is always true → blocks immediately
        assert!(!tracker.check_and_record("key1", 0));
    }

    #[test]
    fn high_limit_allows_many() {
        let tracker = RpmTracker::new();
        for _ in 0..1000 {
            assert!(tracker.check_and_record("power-user", 1000));
        }
        assert!(!tracker.check_and_record("power-user", 1000));
    }
}
