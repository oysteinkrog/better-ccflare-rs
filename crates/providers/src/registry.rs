//! Provider registry — maps provider names to implementations.
//!
//! The registry holds `Arc<dyn Provider>` instances keyed by name.
//! Lookup is O(1) via HashMap.

use std::collections::HashMap;
use std::sync::Arc;

use crate::traits::{OAuthProvider, Provider};

/// Registry of provider implementations.
///
/// Thread-safe via interior Arc wrapping. Clone is cheap.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    oauth_providers: HashMap<String, Arc<dyn OAuthProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider. Replaces any existing provider with the same name.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.name().to_string(), provider);
    }

    /// Register an OAuth provider (also registers it as a regular provider).
    pub fn register_oauth(&mut self, provider: Arc<dyn OAuthProvider>) {
        let name = provider.name().to_string();
        self.providers.insert(name.clone(), provider.clone());
        self.oauth_providers.insert(name, provider);
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    /// Look up an OAuth provider by name.
    pub fn get_oauth(&self, name: &str) -> Option<Arc<dyn OAuthProvider>> {
        self.oauth_providers.get(name).cloned()
    }

    /// List all registered provider names.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<_> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// List all registered OAuth provider names.
    pub fn list_oauth(&self) -> Vec<String> {
        let mut names: Vec<_> = self.oauth_providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// Remove a provider by name. Returns `true` if it was present.
    pub fn unregister(&mut self, name: &str) -> bool {
        let removed = self.providers.remove(name).is_some();
        self.oauth_providers.remove(name);
        removed
    }

    /// Remove all providers.
    pub fn clear(&mut self) {
        self.providers.clear();
        self.oauth_providers.clear();
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

// Custom Debug for Arc<dyn Provider> (can't derive)
impl std::fmt::Debug for dyn Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Provider({})", self.name())
    }
}

impl std::fmt::Debug for dyn OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OAuthProvider({})", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubProvider;

    #[test]
    fn register_and_lookup() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(StubProvider::new("test-provider")));

        assert!(reg.get("test-provider").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn list_providers_sorted() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(StubProvider::new("charlie")));
        reg.register(Arc::new(StubProvider::new("alpha")));
        reg.register(Arc::new(StubProvider::new("bravo")));

        assert_eq!(reg.list(), vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn unregister_provider() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(StubProvider::new("doomed")));
        assert!(reg.unregister("doomed"));
        assert!(!reg.unregister("doomed"));
        assert!(reg.get("doomed").is_none());
    }

    #[test]
    fn replace_existing() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(StubProvider::new("same-name")));
        reg.register(Arc::new(StubProvider::new("same-name")));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn clear_empties_registry() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(StubProvider::new("a")));
        reg.register(Arc::new(StubProvider::new("b")));
        reg.clear();
        assert!(reg.is_empty());
    }
}
