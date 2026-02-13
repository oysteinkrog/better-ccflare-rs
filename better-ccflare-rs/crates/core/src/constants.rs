/// Time constants (all in milliseconds unless noted).
pub mod time {
    pub const SECOND: i64 = 1_000;
    pub const MINUTE: i64 = 60 * SECOND;
    pub const HOUR: i64 = 60 * MINUTE;
    pub const DAY: i64 = 24 * HOUR;

    /// Default Anthropic session duration (5 hours).
    pub const ANTHROPIC_SESSION_DURATION_DEFAULT: i64 = 5 * HOUR;
    /// Fallback Anthropic session duration (1 hour).
    pub const ANTHROPIC_SESSION_DURATION_FALLBACK: i64 = HOUR;
    /// Legacy alias for backward compatibility.
    pub const SESSION_DURATION_DEFAULT: i64 = ANTHROPIC_SESSION_DURATION_DEFAULT;

    pub const STREAM_TIMEOUT_DEFAULT: i64 = MINUTE;
    pub const STREAM_READ_TIMEOUT_MS: i64 = 60_000;
    pub const STREAM_OPERATION_TIMEOUT_MS: i64 = 30_000;
    /// OAuth state TTL in minutes.
    pub const OAUTH_STATE_TTL_MINUTES: i64 = 10;
    pub const RETRY_DELAY_DEFAULT: i64 = SECOND;

    /// HTTP cache header: 365 days in seconds.
    pub const CACHE_YEAR_SECS: i64 = 31_536_000;

    /// API key token expiry: 1 year.
    pub const API_KEY_TOKEN_EXPIRY_MS: i64 = 365 * DAY;
    /// Google token expiry: 1 hour.
    pub const GOOGLE_TOKEN_EXPIRY_MS: i64 = HOUR;
}

/// Buffer sizes in bytes.
pub mod buffer {
    pub const STREAM_USAGE_BUFFER_KB: usize = 64;
    pub const STREAM_USAGE_BUFFER_BYTES: usize = STREAM_USAGE_BUFFER_KB * 1024;

    pub const STREAM_BODY_MAX_KB: usize = 256;
    pub const STREAM_BODY_MAX_BYTES: usize = STREAM_BODY_MAX_KB * 1024;

    pub const ANTHROPIC_STREAM_CAP_BYTES: usize = 32_768;

    pub const STREAM_TEE_MAX_BYTES: usize = 1024 * 1024;

    pub const LOG_FILE_MAX_SIZE: usize = 10 * 1024 * 1024;
}

/// Network constants.
pub mod network {
    pub const DEFAULT_PORT: u16 = 8080;
    pub const IDLE_TIMEOUT_MAX: u64 = 255;
}

/// Cache control header values.
pub mod cache {
    pub const STATIC_ASSETS_MAX_AGE: i64 = 31_536_000;
    pub const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=31536000, immutable";
    pub const CACHE_CONTROL_STATIC: &str = "public, max-age=31536000";
    pub const CACHE_CONTROL_NO_CACHE: &str = "no-cache, no-store, must-revalidate";
}

/// Request/response limits.
pub mod limits {
    pub const REQUEST_HISTORY_DEFAULT: usize = 50;
    pub const REQUEST_DETAILS_DEFAULT: usize = 100;
    pub const REQUEST_HISTORY_MAX: usize = 1000;
    pub const LOG_READ_DEFAULT: usize = 1000;

    pub const ACCOUNT_NAME_MIN_LENGTH: usize = 1;
    pub const ACCOUNT_NAME_MAX_LENGTH: usize = 100;
}

/// HTTP status code constants.
pub mod http_status {
    pub const OK: u16 = 200;
    pub const NOT_FOUND: u16 = 404;
    pub const TOO_MANY_REQUESTS: u16 = 429;
    pub const INTERNAL_SERVER_ERROR: u16 = 500;
    pub const SERVICE_UNAVAILABLE: u16 = 503;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_constants_correct() {
        assert_eq!(time::SECOND, 1_000);
        assert_eq!(time::MINUTE, 60_000);
        assert_eq!(time::HOUR, 3_600_000);
        assert_eq!(time::DAY, 86_400_000);
        assert_eq!(time::ANTHROPIC_SESSION_DURATION_DEFAULT, 18_000_000);
    }

    #[test]
    fn buffer_constants_correct() {
        assert_eq!(buffer::STREAM_USAGE_BUFFER_BYTES, 65_536);
        assert_eq!(buffer::STREAM_BODY_MAX_BYTES, 262_144);
    }

    #[test]
    fn network_constants() {
        assert_eq!(network::DEFAULT_PORT, 8080);
    }
}
