//! Providers crate — AI service provider abstractions for better-ccflare.
//!
//! Defines the [`Provider`] trait, [`ProviderRegistry`], PKCE challenge
//! generation, and model name mapping utilities. Concrete provider
//! implementations (Claude OAuth, Console, etc.) are added in US-006/007/008.

pub mod error;
pub mod model_mapping;
pub mod pkce;
pub mod registry;
pub mod stub;
pub mod traits;
pub mod types;

#[cfg(test)]
pub(crate) mod test_util;

// Re-exports
pub use error::ProviderError;
pub use registry::ProviderRegistry;
pub use traits::{OAuthProvider, Provider, UsageFetcher};
pub use types::{AuthType, RateLimitInfo, TokenRefreshResult, UsageInfo};
