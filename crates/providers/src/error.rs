//! Provider-specific errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Token refresh failed: {0}")]
    TokenRefresh(String),

    #[error("Request build failed: {0}")]
    RequestBuild(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Provider not found: {0}")]
    NotFound(String),

    #[error("PKCE error: {0}")]
    Pkce(String),

    #[error("{0}")]
    Other(String),
}
