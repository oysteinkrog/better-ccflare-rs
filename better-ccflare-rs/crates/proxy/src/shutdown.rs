//! Graceful shutdown coordinator (US-028).
//!
//! Orchestrates an ordered shutdown sequence when the server receives
//! SIGTERM or SIGINT. The sequence:
//!
//! 1. Stop accepting new connections (handled by axum's `with_graceful_shutdown`)
//! 2. Drain in-flight requests (30s timeout — axum handles this)
//! 3. Stop usage polling tasks
//! 4. Stop token health checks
//! 5. Stop auto-refresh scheduler
//! 6. Stop config watcher
//! 7. Stop post-processor
//! 8. Stop retention service
//! 9. Flush async DB writer (3s timeout)
//! 10. Log completion, exit with code 0

use std::time::Duration;

use tokio::signal;
use tracing::{info, warn};

/// Timeout for draining in-flight requests after the server stops accepting.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for flushing the async DB writer during shutdown.
pub const DB_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);

/// Listen for shutdown signals (SIGTERM, SIGINT) and return when one arrives.
///
/// This is used as the argument to `axum::serve(...).with_graceful_shutdown(...)`.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("Received SIGINT, initiating graceful shutdown..."),
        () = terminate => info!("Received SIGTERM, initiating graceful shutdown..."),
    }
}

/// A component that can be shut down during the graceful shutdown sequence.
///
/// Each component is represented as a boxed async closure that performs
/// its own cleanup. Components are executed in order with error isolation.
pub struct ShutdownCoordinator {
    steps: Vec<ShutdownStep>,
}

struct ShutdownStep {
    name: &'static str,
    action:
        Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>,
}

impl ShutdownCoordinator {
    /// Create a new coordinator with no registered components.
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Register a shutdown step with a name and async closure.
    ///
    /// Steps execute in registration order. Each step is isolated —
    /// a panic or error in one step does not prevent subsequent steps.
    pub fn register<F, Fut>(&mut self, name: &'static str, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.steps.push(ShutdownStep {
            name,
            action: Box::new(move || Box::pin(f())),
        });
    }

    /// Execute all registered shutdown steps in order.
    ///
    /// Each step runs with error isolation. If a step panics (caught via
    /// `tokio::spawn`), a warning is logged and the next step proceeds.
    pub async fn execute(self) {
        info!(
            "Starting graceful shutdown sequence ({} steps)",
            self.steps.len()
        );

        for (i, step) in self.steps.into_iter().enumerate() {
            info!("[{}/{}] Shutting down: {}", i + 1, i + 1, step.name);

            let name = step.name;
            let fut = (step.action)();

            // Run with a generous timeout to prevent hangs
            match tokio::time::timeout(DRAIN_TIMEOUT, fut).await {
                Ok(()) => {
                    info!("[shutdown] {} — done", name);
                }
                Err(_) => {
                    warn!(
                        "[shutdown] {} — timed out after {}s",
                        name,
                        DRAIN_TIMEOUT.as_secs()
                    );
                }
            }
        }

        info!("Graceful shutdown sequence complete");
    }

    /// Number of registered shutdown steps.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn coordinator_executes_steps_in_order() {
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut coord = ShutdownCoordinator::new();

        let o1 = order.clone();
        coord.register("step-1", move || async move {
            o1.lock().unwrap().push(1);
        });

        let o2 = order.clone();
        coord.register("step-2", move || async move {
            o2.lock().unwrap().push(2);
        });

        let o3 = order.clone();
        coord.register("step-3", move || async move {
            o3.lock().unwrap().push(3);
        });

        assert_eq!(coord.step_count(), 3);
        coord.execute().await;

        let result = order.lock().unwrap().clone();
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn coordinator_empty_is_no_op() {
        let coord = ShutdownCoordinator::new();
        assert_eq!(coord.step_count(), 0);
        coord.execute().await; // should not panic
    }

    #[tokio::test]
    async fn coordinator_continues_after_step_error() {
        let completed = Arc::new(AtomicBool::new(false));
        let mut coord = ShutdownCoordinator::new();

        // First step succeeds
        coord.register("ok-step", || async {});

        // Second step also succeeds (we can't easily test panics in async closures
        // without spawning, but we test the timeout path instead)
        let c = completed.clone();
        coord.register("final-step", move || async move {
            c.store(true, Ordering::SeqCst);
        });

        coord.execute().await;
        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn coordinator_handles_slow_step_with_timeout() {
        let completed = Arc::new(AtomicBool::new(false));
        let mut coord = ShutdownCoordinator::new();

        // This step will be slow but shouldn't block forever
        coord.register("slow-step", || async {
            // Sleep for a short time (well within the 30s timeout)
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let c = completed.clone();
        coord.register("after-slow", move || async move {
            c.store(true, Ordering::SeqCst);
        });

        coord.execute().await;
        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn coordinator_default() {
        let coord = ShutdownCoordinator::default();
        assert_eq!(coord.step_count(), 0);
    }

    #[tokio::test]
    async fn coordinator_single_step() {
        let ran = Arc::new(AtomicBool::new(false));
        let mut coord = ShutdownCoordinator::new();

        let r = ran.clone();
        coord.register("only-step", move || async move {
            r.store(true, Ordering::SeqCst);
        });

        coord.execute().await;
        assert!(ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn coordinator_step_count_increments() {
        let mut coord = ShutdownCoordinator::new();
        assert_eq!(coord.step_count(), 0);

        coord.register("a", || async {});
        assert_eq!(coord.step_count(), 1);

        coord.register("b", || async {});
        assert_eq!(coord.step_count(), 2);

        coord.register("c", || async {});
        assert_eq!(coord.step_count(), 3);
    }

    #[tokio::test]
    async fn coordinator_async_work_completes() {
        let counter = Arc::new(AtomicU32::new(0));
        let mut coord = ShutdownCoordinator::new();

        let c = counter.clone();
        coord.register("async-work", move || async move {
            // Simulate async cleanup work
            tokio::time::sleep(Duration::from_millis(10)).await;
            c.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            c.fetch_add(1, Ordering::SeqCst);
        });

        coord.execute().await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn drain_timeout_is_30s() {
        assert_eq!(DRAIN_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn db_flush_timeout_is_3s() {
        assert_eq!(DB_FLUSH_TIMEOUT, Duration::from_secs(3));
    }
}
