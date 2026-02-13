//! Exponential backoff retry for SQLITE_BUSY / SQLITE_LOCKED.
//!
//! Mirrors the TypeScript retry logic: 3 attempts, 100ms initial delay,
//! 2× backoff, 5000ms max, 10% jitter.

use crate::error::DbError;
use std::thread;
use std::time::Duration;

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the first).
    pub max_attempts: u32,
    /// Initial delay in milliseconds.
    pub initial_delay_ms: u64,
    /// Backoff multiplier applied after each retry.
    pub backoff_factor: f64,
    /// Maximum delay in milliseconds.
    pub max_delay_ms: u64,
    /// Jitter factor (0.0–1.0). Applied as ±jitter_factor of the delay.
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 100,
            backoff_factor: 2.0,
            max_delay_ms: 5_000,
            jitter_factor: 0.1,
        }
    }
}

impl RetryConfig {
    /// Compute the delay for a given attempt number (0-indexed).
    fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay_ms as f64 * self.backoff_factor.powi(attempt as i32);
        let capped = base.min(self.max_delay_ms as f64);

        // Apply jitter: ±jitter_factor
        let jitter_range = capped * self.jitter_factor;
        // Deterministic jitter based on attempt number (avoids needing rand crate)
        let jitter = jitter_range * (((attempt as f64 * 7.0) % 20.0) / 10.0 - 1.0);
        let final_ms = (capped + jitter).max(0.0);

        Duration::from_millis(final_ms as u64)
    }
}

/// Execute a closure with retry on SQLITE_BUSY/LOCKED errors.
pub fn with_retry<T, F>(config: &RetryConfig, mut f: F) -> Result<T, DbError>
where
    F: FnMut() -> Result<T, DbError>,
{
    let mut last_err = None;

    for attempt in 0..config.max_attempts {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) if e.is_busy() && attempt + 1 < config.max_attempts => {
                let delay = config.delay_for_attempt(attempt);
                tracing::warn!(
                    attempt = attempt + 1,
                    max_attempts = config.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    "Database busy, retrying"
                );
                thread::sleep(delay);
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap_or(DbError::BusyTimeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn retry_succeeds_on_first_try() {
        let config = RetryConfig::default();
        let result = with_retry(&config, || Ok::<_, DbError>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn retry_succeeds_on_second_try() {
        let config = RetryConfig::default();
        let count = AtomicU32::new(0);
        let result = with_retry(&config, || {
            let n = count.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    None,
                )))
            } else {
                Ok(99)
            }
        });
        assert_eq!(result.unwrap(), 99);
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn retry_fails_after_max_attempts() {
        let config = RetryConfig {
            max_attempts: 2,
            initial_delay_ms: 1, // fast for tests
            ..Default::default()
        };
        let count = AtomicU32::new(0);
        let result = with_retry(&config, || {
            count.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>(DbError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )))
        });
        assert!(result.is_err());
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn non_busy_errors_not_retried() {
        let config = RetryConfig::default();
        let count = AtomicU32::new(0);
        let result = with_retry(&config, || {
            count.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>(DbError::Other("not busy".into()))
        });
        assert!(result.is_err());
        // Should only be called once — non-busy errors are not retried
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn delay_increases_with_backoff() {
        let config = RetryConfig::default();
        let d0 = config.delay_for_attempt(0);
        let d1 = config.delay_for_attempt(1);
        // With jitter, exact values vary, but d1 should be roughly 2× d0
        assert!(d1.as_millis() > d0.as_millis());
    }
}
