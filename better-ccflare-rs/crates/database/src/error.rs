//! Database-specific error types.

/// Database errors — wraps rusqlite, pool, and migration failures.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Connection pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database busy after retries")]
    BusyTimeout,

    #[error("{0}")]
    Other(String),
}

impl DbError {
    pub fn migration(msg: impl Into<String>) -> Self {
        Self::Migration(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    /// Whether this error represents a retryable SQLITE_BUSY/LOCKED condition.
    pub fn is_busy(&self) -> bool {
        match self {
            Self::Sqlite(e) => matches!(
                e.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ),
            _ => false,
        }
    }
}
