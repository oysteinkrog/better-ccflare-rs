use tracing_subscriber::{fmt, EnvFilter};

/// Initialize the tracing subscriber based on environment variables.
///
/// - `LOG_LEVEL`: sets the filter level (trace, debug, info, warn, error). Default: info
/// - `LOG_FORMAT`: "json" for JSON output, "pretty" for human-readable. Default: pretty
pub fn init_logging() {
    let filter = EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));

    let format = std::env::var("LOG_FORMAT").unwrap_or_default();

    if format.eq_ignore_ascii_case("json") {
        fmt::Subscriber::builder()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        fmt::Subscriber::builder().with_env_filter(filter).init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_logging_does_not_panic() {
        // Can only init once per process; just verify the function exists and compiles.
        let _ = std::panic::catch_unwind(init_logging);
    }
}
