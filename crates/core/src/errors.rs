use serde::Serialize;
use std::fmt;

/// HTTP status code type alias.
pub type StatusCode = u16;

/// Base application error with HTTP status mapping.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{message}")]
    Auth {
        message: String,
        code: &'static str,
        status_code: StatusCode,
        context: Option<ErrorContext>,
    },

    #[error("Failed to refresh access token")]
    TokenRefresh {
        account_id: String,
        original_error: Option<String>,
    },

    #[error("Rate limit exceeded")]
    RateLimit {
        account_id: String,
        reset_time: i64,
        remaining: Option<i64>,
    },

    #[error("{message}")]
    Validation {
        message: String,
        field: Option<String>,
        value: Option<String>,
    },

    #[error("{message}")]
    Provider {
        message: String,
        provider: String,
        status_code: StatusCode,
        context: Option<ErrorContext>,
    },

    #[error("{message}")]
    OAuth {
        message: String,
        provider: String,
        oauth_code: Option<String>,
    },

    #[error("{message}")]
    ServiceUnavailable {
        message: String,
        service: Option<String>,
    },

    #[error("{message}")]
    Http {
        status: StatusCode,
        message: String,
        details: Option<String>,
    },

    #[error("{message}")]
    Internal { message: String },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Contextual data attached to errors.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorContext {
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl AppError {
    // -- Constructors --

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
            code: "AUTH_ERROR",
            status_code: 401,
            context: None,
        }
    }

    pub fn token_refresh(account_id: impl Into<String>, original: Option<String>) -> Self {
        Self::TokenRefresh {
            account_id: account_id.into(),
            original_error: original,
        }
    }

    pub fn rate_limit(
        account_id: impl Into<String>,
        reset_time: i64,
        remaining: Option<i64>,
    ) -> Self {
        Self::RateLimit {
            account_id: account_id.into(),
            reset_time,
            remaining,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            field: None,
            value: None,
        }
    }

    pub fn validation_field(message: impl Into<String>, field: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            field: Some(field.into()),
            value: None,
        }
    }

    pub fn provider(
        message: impl Into<String>,
        provider: impl Into<String>,
        status_code: StatusCode,
    ) -> Self {
        Self::Provider {
            message: message.into(),
            provider: provider.into(),
            status_code,
            context: None,
        }
    }

    pub fn oauth(
        message: impl Into<String>,
        provider: impl Into<String>,
        oauth_code: Option<String>,
    ) -> Self {
        Self::OAuth {
            message: message.into(),
            provider: provider.into(),
            oauth_code,
        }
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            message: message.into(),
            service: None,
        }
    }

    pub fn http(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Http {
            status,
            message: message.into(),
            details: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    // -- Status code mapping --

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Auth { status_code, .. } => *status_code,
            Self::TokenRefresh { .. } => 401,
            Self::RateLimit { .. } => 429,
            Self::Validation { .. } => 400,
            Self::Provider { status_code, .. } => *status_code,
            Self::OAuth { .. } => 400,
            Self::ServiceUnavailable { .. } => 503,
            Self::Http { status, .. } => *status,
            Self::Internal { .. } => 500,
            Self::Other(_) => 500,
        }
    }

    pub fn error_code(&self) -> &str {
        match self {
            Self::Auth { code, .. } => code,
            Self::TokenRefresh { .. } => "AUTH_ERROR",
            Self::RateLimit { .. } => "RATE_LIMIT_ERROR",
            Self::Validation { .. } => "VALIDATION_ERROR",
            Self::Provider { .. } => "PROVIDER_ERROR",
            Self::OAuth { .. } => "PROVIDER_ERROR",
            Self::ServiceUnavailable { .. } => "SERVICE_UNAVAILABLE",
            Self::Http { .. } => "HTTP_ERROR",
            Self::Internal { .. } => "INTERNAL_ERROR",
            Self::Other(_) => "INTERNAL_ERROR",
        }
    }
}

/// Serializable error response for the API.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl From<&AppError> for ErrorResponse {
    fn from(err: &AppError) -> Self {
        Self {
            error: ErrorBody {
                code: err.error_code().to_string(),
                message: err.to_string(),
                details: None,
            },
        }
    }
}

/// Error type classification for pattern matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorType {
    Network,
    Auth,
    RateLimit,
    Validation,
    Server,
    Unknown,
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => write!(f, "network"),
            Self::Auth => write!(f, "auth"),
            Self::RateLimit => write!(f, "rate-limit"),
            Self::Validation => write!(f, "validation"),
            Self::Server => write!(f, "server"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl ErrorType {
    /// Classify an `AppError` into an `ErrorType`.
    pub fn from_app_error(err: &AppError) -> Self {
        match err {
            AppError::Auth { .. } | AppError::TokenRefresh { .. } => Self::Auth,
            AppError::RateLimit { .. } => Self::RateLimit,
            AppError::Validation { .. } => Self::Validation,
            AppError::ServiceUnavailable { .. } => Self::Server,
            AppError::Http { status, .. } => {
                if *status == 401 {
                    Self::Auth
                } else if *status == 429 {
                    Self::RateLimit
                } else if *status >= 400 && *status < 500 {
                    Self::Validation
                } else if *status >= 500 {
                    Self::Server
                } else {
                    Self::Unknown
                }
            }
            AppError::Provider { .. } | AppError::OAuth { .. } => Self::Server,
            AppError::Internal { .. } | AppError::Other(_) => Self::Server,
        }
    }

    pub fn default_message(&self) -> &'static str {
        match self {
            Self::Network => "Network error. Please check your connection and try again.",
            Self::Auth => "Authentication failed. Please sign in again.",
            Self::RateLimit => "Too many requests. Please try again later.",
            Self::Validation => "Invalid request. Please check your input.",
            Self::Server => "Server error. Please try again later.",
            Self::Unknown => "An unexpected error occurred.",
        }
    }
}

/// Sanitize error context to remove sensitive data.
pub fn sanitize_context(
    context: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    const SENSITIVE_KEYS: &[&str] = &["token", "password", "secret", "key", "authorization"];

    let mut sanitized = serde_json::Map::new();
    for (key, value) in context {
        let lower_key = key.to_lowercase();
        if SENSITIVE_KEYS.iter().any(|s| lower_key.contains(s)) {
            sanitized.insert(key.clone(), serde_json::Value::String("[REDACTED]".into()));
        } else if let serde_json::Value::Object(obj) = value {
            sanitized.insert(
                key.clone(),
                serde_json::Value::Object(sanitize_context(obj)),
            );
        } else {
            sanitized.insert(key.clone(), value.clone());
        }
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_status_code() {
        let err = AppError::auth("unauthorized");
        assert_eq!(err.status_code(), 401);
        assert_eq!(err.error_code(), "AUTH_ERROR");
    }

    #[test]
    fn rate_limit_error_status_code() {
        let err = AppError::rate_limit("acc-1", 1234567890, Some(0));
        assert_eq!(err.status_code(), 429);
        assert_eq!(err.error_code(), "RATE_LIMIT_ERROR");
    }

    #[test]
    fn validation_error_status_code() {
        let err = AppError::validation("bad input");
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn provider_error_status_code() {
        let err = AppError::provider("upstream fail", "anthropic", 502);
        assert_eq!(err.status_code(), 502);
    }

    #[test]
    fn service_unavailable_status_code() {
        let err = AppError::service_unavailable("down");
        assert_eq!(err.status_code(), 503);
    }

    #[test]
    fn error_type_classification() {
        assert_eq!(
            ErrorType::from_app_error(&AppError::auth("x")),
            ErrorType::Auth
        );
        assert_eq!(
            ErrorType::from_app_error(&AppError::rate_limit("a", 0, None)),
            ErrorType::RateLimit
        );
        assert_eq!(
            ErrorType::from_app_error(&AppError::validation("x")),
            ErrorType::Validation
        );
    }

    #[test]
    fn sanitize_context_redacts_sensitive() {
        let mut ctx = serde_json::Map::new();
        ctx.insert(
            "access_token".into(),
            serde_json::Value::String("secret123".into()),
        );
        ctx.insert("name".into(), serde_json::Value::String("test".into()));
        let sanitized = sanitize_context(&ctx);
        assert_eq!(sanitized["access_token"], "[REDACTED]");
        assert_eq!(sanitized["name"], "test");
    }

    #[test]
    fn error_response_serialization() {
        let err = AppError::validation("field required");
        let resp = ErrorResponse::from(&err);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("VALIDATION_ERROR"));
        assert!(json.contains("field required"));
    }
}
