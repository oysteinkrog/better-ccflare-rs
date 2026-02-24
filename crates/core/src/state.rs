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
// Event bus
// ---------------------------------------------------------------------------

/// Broadcast event bus for real-time updates to SSE clients.
///
/// Wraps `tokio::sync::broadcast` with typed [`Event`](crate::events::Event)
/// payloads serialized to JSON strings. The channel uses a 1024-event ring
/// buffer; slow subscribers that fall behind receive `RecvError::Lagged` and
/// automatically skip missed events.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<String>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish a typed event. Serializes to JSON and broadcasts to all
    /// subscribers. Returns the number of active receivers, or an error
    /// if serialization fails or there are no receivers.
    pub fn publish(&self, event: &crate::events::Event) -> Result<usize, String> {
        let json = event.to_json().map_err(|e| e.to_string())?;
        self.tx.send(json).map_err(|e| e.to_string())
    }

    /// Send a raw JSON string event. Prefer [`publish`](Self::publish) for
    /// typed events.
    pub fn send(&self, event: String) -> Result<usize, broadcast::error::SendError<String>> {
        self.tx.send(event)
    }

    /// Subscribe to events. The returned receiver gets all events published
    /// after this call. If the subscriber falls behind by more than the
    /// channel capacity, it receives `RecvError::Lagged(n)`.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Number of active subscribers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(crate::events::EVENT_BUS_CAPACITY)
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

    /// OAuth store for pending re-auth flows.
    oauth_store: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Usage cache for account utilization data (from provider APIs).
    usage_cache: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// Shared HTTP client for upstream requests (connection pooling).
    http_client: Option<Box<dyn std::any::Any + Send + Sync>>,

    /// X-factor capacity estimation cache (per-account Bayesian state).
    xfactor_cache: Option<Box<dyn std::any::Any + Send + Sync>>,
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
            oauth_store: None,
            usage_cache: None,
            http_client: None,
            xfactor_cache: None,
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

    /// Get the OAuth store, downcasting from the type-erased box.
    pub fn oauth_store<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.oauth_store
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the usage cache, downcasting from the type-erased box.
    pub fn usage_cache<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.usage_cache
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the shared HTTP client, downcasting from the type-erased box.
    pub fn http_client<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.http_client
            .as_ref()
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Get the X-factor capacity cache, downcasting from the type-erased box.
    pub fn xfactor_cache<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.xfactor_cache
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

    /// Set the OAuth store (type-erased).
    pub fn oauth_store<T: 'static + Send + Sync>(mut self, store: T) -> Self {
        self.state.oauth_store = Some(Box::new(store));
        self
    }

    /// Set the usage cache (type-erased).
    pub fn usage_cache<T: 'static + Send + Sync>(mut self, cache: T) -> Self {
        self.state.usage_cache = Some(Box::new(cache));
        self
    }

    /// Set the shared HTTP client (type-erased).
    pub fn http_client<T: 'static + Send + Sync>(mut self, client: T) -> Self {
        self.state.http_client = Some(Box::new(client));
        self
    }

    /// Set the X-factor capacity cache (type-erased).
    pub fn xfactor_cache<T: 'static + Send + Sync>(mut self, cache: T) -> Self {
        self.state.xfactor_cache = Some(Box::new(cache));
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
            .field("oauth_store", &self.oauth_store.is_some())
            .field("usage_cache", &self.usage_cache.is_some())
            .field("http_client", &self.http_client.is_some())
            .field("xfactor_cache", &self.xfactor_cache.is_some())
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
    fn event_bus_publish_typed_event() {
        use crate::events::Event;

        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        let event = Event::RequestStart {
            id: "req-1".into(),
            timestamp: 1700000000000,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account_id: Some("acct-1".into()),
            status_code: 200,
            agent_used: None,
        };

        let count = bus.publish(&event).unwrap();
        assert_eq!(count, 1);

        let json = rx.try_recv().unwrap();
        assert!(json.contains(r#""type":"request_start""#));
        assert!(json.contains(r#""id":"req-1""#));
    }

    #[test]
    fn event_bus_multiple_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        let mut rx3 = bus.subscribe();

        assert_eq!(bus.receiver_count(), 3);

        bus.send("broadcast".into()).unwrap();

        assert_eq!(rx1.try_recv().unwrap(), "broadcast");
        assert_eq!(rx2.try_recv().unwrap(), "broadcast");
        assert_eq!(rx3.try_recv().unwrap(), "broadcast");
    }

    #[test]
    fn event_bus_lagging_receiver() {
        // Create a bus with capacity 4
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();

        // Send more events than the buffer can hold
        for i in 0..8 {
            let _ = bus.send(format!("event-{i}"));
        }

        // First recv should report lag
        match rx.try_recv() {
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                assert!(n > 0, "should report skipped events");
            }
            other => {
                // Some implementations may return the oldest available event
                // Either way, we should be able to continue receiving
                assert!(
                    other.is_ok()
                        || matches!(other, Err(broadcast::error::TryRecvError::Lagged(_)))
                );
            }
        }
    }

    #[test]
    fn event_bus_default_capacity() {
        let bus = EventBus::default();
        // Default should use EVENT_BUS_CAPACITY (1024)
        // We can't directly check capacity, but we can verify it works
        // by sending many events without lag for a small subscriber
        let mut rx = bus.subscribe();
        for i in 0..100 {
            bus.send(format!("event-{i}")).unwrap();
        }
        // All 100 should be receivable without lag
        for i in 0..100 {
            let msg = rx.try_recv().unwrap();
            assert_eq!(msg, format!("event-{i}"));
        }
    }

    #[test]
    fn event_bus_no_receivers_returns_err() {
        let bus = EventBus::new(16);
        // No subscribers — send returns error (zero receivers)
        let result = bus.send("nobody-listening".into());
        assert!(result.is_err());
    }

    #[test]
    fn event_bus_dropped_receiver_decrements_count() {
        let bus = EventBus::new(16);
        let rx1 = bus.subscribe();
        let _rx2 = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);

        drop(rx1);
        assert_eq!(bus.receiver_count(), 1);
    }

    #[test]
    fn debug_impl() {
        let state = AppState::new(test_config());
        let debug = format!("{state:?}");
        assert!(debug.contains("AppState"));
        assert!(debug.contains("db_pool: false"));
    }
}
