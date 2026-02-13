//! Provider-specific errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Token refresh failed: {0}")]
    TokenRefresh(String),

    #[error("Request build failed: {0}")]
    RequestBuild(String),

    #[error("Request transform failed: {0}")]
    RequestTransform(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Provider not found: {0}")]
    NotFound(String),

    #[error("PKCE error: {0}")]
    Pkce(String),

    #[error("{0}")]
    Other(String),
}
