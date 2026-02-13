//! Connection pool and SQLite pragma configuration.
//!
//! Uses r2d2 for connection pooling. Each connection is initialized with
//! WAL mode and performance pragmas matching the TypeScript implementation.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use crate::error::DbError;
use crate::migrations;
use crate::schema;

/// Type alias for the connection pool.
pub type DbPool = Pool<SqliteConnectionManager>;

/// Pool configuration.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of connections in the pool.
    pub max_size: u32,
    /// Minimum idle connections to keep in the pool.
    pub min_idle: Option<u32>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 5,
            min_idle: Some(1),
        }
    }
}

/// Apply SQLite pragmas to a connection.
///
/// These match the TypeScript implementation's `configurePragmas()`.
pub fn apply_pragmas(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA busy_timeout = 10000;
        PRAGMA cache_size = -10000;
        PRAGMA synchronous = FULL;
        PRAGMA mmap_size = 0;
        PRAGMA temp_store = MEMORY;
        PRAGMA foreign_keys = ON;
        PRAGMA wal_autocheckpoint = 1000;
        ",
    )?;
    Ok(())
}

/// Create a connection pool, applying pragmas and initializing the schema.
///
/// On first run:
/// 1. Migrates legacy `ccflare.db` if it exists and new DB does not
/// 2. Creates a timestamped backup of an existing DB before schema changes
/// 3. Creates all tables and indexes
pub fn create_pool(db_path: &std::path::Path, config: &PoolConfig) -> Result<DbPool, DbError> {
    // Ensure directory exists
    crate::paths::ensure_db_dir(db_path)?;

    // Migrate from legacy location if needed
    migrations::migrate_from_legacy(db_path)?;

    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::builder()
        .max_size(config.max_size)
        .min_idle(config.min_idle)
        .connection_customizer(Box::new(PragmaCustomizer))
        .build(manager)?;

    // Initialize schema on a fresh connection
    let conn = pool.get()?;
    let already_initialized = migrations::is_initialized(&conn);

    if already_initialized {
        // Backup before potential schema changes
        let _ = migrations::backup_existing_db(db_path);
    }

    schema::create_tables(&conn)?;
    schema::create_indexes(&conn)?;

    tracing::info!(
        path = %db_path.display(),
        pool_size = config.max_size,
        "Database pool initialized"
    );

    Ok(pool)
}

/// Create an in-memory pool (for testing).
pub fn create_memory_pool(config: &PoolConfig) -> Result<DbPool, DbError> {
    let manager = SqliteConnectionManager::memory();
    let pool = Pool::builder()
        .max_size(config.max_size)
        .min_idle(config.min_idle)
        .connection_customizer(Box::new(PragmaCustomizer))
        .build(manager)?;

    let conn = pool.get()?;
    schema::create_tables(&conn)?;
    schema::create_indexes(&conn)?;

    Ok(pool)
}

/// r2d2 connection customizer that applies pragmas on each new connection.
#[derive(Debug)]
struct PragmaCustomizer;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for PragmaCustomizer {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), rusqlite::Error> {
        apply_pragmas(conn).map_err(|e| match e {
            DbError::Sqlite(e) => e,
            other => rusqlite::Error::InvalidParameterName(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_pool_creates_successfully() {
        let pool = create_memory_pool(&PoolConfig::default()).unwrap();
        let conn = pool.get().unwrap();
        // Verify pragmas were applied
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        // In-memory databases use "memory" journal mode regardless of WAL setting
        assert!(
            journal_mode == "wal" || journal_mode == "memory",
            "Unexpected journal mode: {journal_mode}"
        );
    }

    #[test]
    fn memory_pool_has_tables() {
        let pool = create_memory_pool(&PoolConfig::default()).unwrap();
        let conn = pool.get().unwrap();
        assert!(migrations::is_initialized(&conn));
    }

    #[test]
    fn pragmas_applied_correctly() {
        let conn = Connection::open_in_memory().unwrap();
        apply_pragmas(&conn).unwrap();

        let busy_timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 10000);

        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        // synchronous=FULL is value 2
        assert_eq!(synchronous, 2);

        let temp_store: i64 = conn
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .unwrap();
        // temp_store=MEMORY is value 2
        assert_eq!(temp_store, 2);
    }

    #[test]
    fn pool_config_default() {
        let config = PoolConfig::default();
        assert_eq!(config.max_size, 5);
        assert_eq!(config.min_idle, Some(1));
    }
}
