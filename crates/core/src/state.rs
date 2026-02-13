//! Shared application state for axum handler access.
//!
//! `AppState` ties all components together. Handlers receive it via
//! `axum::extract::State<Arc<AppState>>`. Hot-reloadable config uses
//! `arc_swap::ArcSwap` so readers never block.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::broadcast;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Event bus (placeholder — US-017 will expand)
// ---------------------------------------------------------------------------

/// Broadcast event bus for real-time updates (SSE, WebSocket, etc.).
///
/// For now this is a simple wrapper around `tokio::sync::broadcast`.
/// US-017 will add typed events and subscriber helpers.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<String>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event to all subscribers.
    pub fn send(&self, event: String) -> Result<usize, broadcast::error::SendError<String>> {
        self.tx.send(event)
    }

    /// Subscribe to events.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Shared application state accessible from all axum handlers.
///
/// All fields are cheaply cloneable (`Arc`, channels, pool handles).
/// Wrap in `Arc<AppState>` and pass to axum's `State` extractor.
///
/// Components not yet implemented are represented as `Option` fields
/// so the struct compiles and tests pass before those beads land.
///
/// ```rust,ignore
/// async fn handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
///     let config = state.config();
///     // ...
/// }
/// ```
pub struct AppState {
    /// Atomically swappable config for hot-reload (US-032).
    config: Arc<ArcSwap<Config>>,

    /// Broadcast event bus for real-time updates.
    pub event_bus: Arc<EventBus>,

    // -- Fields below are set via builder; Option means "not yet wired" --
    /// Database connection pool (from bccf-database).
    /// Stored as `Box<dyn Any + Send + Sync>` to avoid coupling core to r2d2.
    db_pool: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Async database writer handle (from bccf-database).
    async_writer: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Provider registry (US-005).
    provider_registry: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Load balancer / session strategy (US-009).
    load_balancer: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Token manager (US-010).
    token_manager: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl AppState {
    /// Create a new AppState with the given config.
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(ArcSwap::new(Arc::new(config))),
            event_bus: Arc::new(EventBus::default()),
            db_pool: None,
            async_writer: None,
            provider_registry: None,
            load_balancer: None,
            token_manager: None,
        }
    }

    /// Get a snapshot of the current config. The returned `Arc<Config>` is
    /// valid even if another thread swaps the config concurrently.
    pub fn config(&self) -> arc_swap::Guard<Arc<Config>> {
        self.config.load()
    }

    /// Atomically swap the config. Existing readers keep their snapshot.
    pub fn swap_config(&self, new_config: Config) {
        self.config.store(Arc::new(new_config));
    }

    /// Access the raw ArcSwap for advanced patterns (e.g. `rcu`).
    pub fn config_swap(&self) -> &Arc<ArcSwap<Config>> {
        &self.config
    }

    /// Get the database pool, downcasting from the type-erased box.
    ///
    /// Returns `None` if the pool hasn't been set or the type doesn't match.
    pub fn db_pool<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.db_pool.as_ref().and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the async database writer, downcasting from the type-erased box.
    pub fn async_writer<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.async_writer
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the provider registry, downcasting from the type-erased box.
    pub fn provider_registry<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.provider_registry
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the load balancer, downcasting from the type-erased box.
    pub fn load_balancer<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.load_balancer
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the token manager, downcasting from the type-erased box.
    pub fn token_manager<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.token_manager
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for `AppState` — allows setting optional components.
pub struct AppStateBuilder {
    state: AppState,
}

impl AppStateBuilder {
    pub fn new(config: Config) -> Self {
        Self {
            state: AppState::new(config),
        }
    }

    /// Set the event bus (overrides the default).
    pub fn event_bus(mut self, bus: EventBus) -> Self {
        self.state.event_bus = Arc::new(bus);
        self
    }

    /// Set the database pool (type-erased).
    pub fn db_pool<T: 'static + Send + Sync>(mut self, pool: T) -> Self {
        self.state.db_pool = Some(Box::new(pool));
        self
    }

    /// Set the async database writer (type-erased).
    pub fn async_writer<T: 'static + Send + Sync>(mut self, writer: T) -> Self {
        self.state.async_writer = Some(Box::new(writer));
        self
    }

    /// Set the provider registry (type-erased).
    pub fn provider_registry<T: 'static + Send + Sync>(mut self, registry: T) -> Self {
        self.state.provider_registry = Some(Box::new(registry));
        self
    }

    /// Set the load balancer (type-erased).
    pub fn load_balancer<T: 'static + Send + Sync>(mut self, lb: T) -> Self {
        self.state.load_balancer = Some(Box::new(lb));
        self
    }

    /// Set the token manager (type-erased).
    pub fn token_manager<T: 'static + Send + Sync>(mut self, tm: T) -> Self {
        self.state.token_manager = Some(Box::new(tm));
        self
    }

    /// Build the final `AppState`.
    pub fn build(self) -> AppState {
        self.state
    }
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &"<ArcSwap<Config>>")
            .field("event_bus", &self.event_bus)
            .field("db_pool", &self.db_pool.is_some())
            .field("async_writer", &self.async_writer.is_some())
            .field("provider_registry", &self.provider_registry.is_some())
            .field("load_balancer", &self.load_balancer.is_some())
            .field("token_manager", &self.token_manager.is_some())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config() -> Config {
        Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-state-nonexistent/config.json",
        )))
        .unwrap()
    }

    #[test]
    fn construct_default_app_state() {
        let state = AppState::new(test_config());
        assert!(state.db_pool::<String>().is_none());
        assert!(state.async_writer::<String>().is_none());
        assert!(state.provider_registry::<String>().is_none());
        assert!(state.load_balancer::<String>().is_none());
        assert!(state.token_manager::<String>().is_none());
    }

    #[test]
    fn config_load_and_swap() {
        let state = AppState::new(test_config());
        let port_before = state.config().get_runtime().port;
        assert_eq!(port_before, 8080);

        // Swap with a new config — snapshot still works
        let _guard = state.config();
        state.swap_config(test_config());
        // Old guard still valid (Arc-based)
    }

    #[test]
    fn builder_sets_components() {
        let state = AppStateBuilder::new(test_config())
            .db_pool("mock-pool".to_string())
            .async_writer(42_u64)
            .build();

        assert_eq!(state.db_pool::<String>(), Some(&"mock-pool".to_string()));
        assert_eq!(state.async_writer::<u64>(), Some(&42));
        assert!(state.provider_registry::<String>().is_none());
    }

    #[test]
    fn downcast_wrong_type_returns_none() {
        let state = AppStateBuilder::new(test_config())
            .db_pool("pool".to_string())
            .build();

        // Wrong type → None
        assert!(state.db_pool::<u64>().is_none());
    }

    #[test]
    fn event_bus_send_and_receive() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        bus.send("test-event".into()).unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, "test-event");
    }

    #[test]
    fn debug_impl() {
        let state = AppState::new(test_config());
        let debug = format!("{state:?}");
        assert!(debug.contains("AppState"));
        assert!(debug.contains("db_pool: false"));
    }
}
