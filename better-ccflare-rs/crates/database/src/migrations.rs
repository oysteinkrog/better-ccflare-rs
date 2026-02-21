//! Database migrations and backup logic.
//!
//! On first run with an existing DB, creates a timestamped backup before
//! applying schema. Also handles legacy ccflare.db detection and copy.

use std::path::Path;

use crate::error::DbError;
use crate::paths;

/// Copy legacy `~/.config/ccflare/ccflare.db` to the new location if it
/// exists and the new database does not yet exist.
pub fn migrate_from_legacy(db_path: &Path) -> Result<bool, DbError> {
    if db_path.exists() {
        return Ok(false);
    }

    let legacy_path = paths::resolve_legacy_db_path();
    if !legacy_path.exists() {
        return Ok(false);
    }

    tracing::info!(
        legacy = %legacy_path.display(),
        target = %db_path.display(),
        "Migrating legacy ccflare database"
    );

    // Ensure target directory exists
    paths::ensure_db_dir(db_path)?;

    // Copy main DB file
    std::fs::copy(&legacy_path, db_path)?;

    // Also copy WAL and SHM if present
    let legacy_wal = legacy_path.with_extension("db-wal");
    let legacy_shm = legacy_path.with_extension("db-shm");
    let target_wal = db_path.with_extension("db-wal");
    let target_shm = db_path.with_extension("db-shm");

    if legacy_wal.exists() {
        let _ = std::fs::copy(&legacy_wal, &target_wal);
    }
    if legacy_shm.exists() {
        let _ = std::fs::copy(&legacy_shm, &target_shm);
    }

    tracing::info!("Legacy database migration complete");
    Ok(true)
}

/// Create a timestamped backup of an existing database file.
///
/// Returns the backup path if a backup was created.
pub fn backup_existing_db(db_path: &Path) -> Result<Option<std::path::PathBuf>, DbError> {
    if !db_path.exists() {
        return Ok(None);
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let backup_name = format!(
        "{}.bak.{timestamp}",
        db_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let backup_path = db_path.with_file_name(backup_name);

    tracing::info!(
        source = %db_path.display(),
        backup = %backup_path.display(),
        "Creating database backup"
    );

    std::fs::copy(db_path, &backup_path)?;
    Ok(Some(backup_path))
}

/// Copy the Node/TS-era database to the RS location on first run.
///
/// On startup, if the RS DB (`better-ccflare-rs.db`) does NOT exist but the
/// Node DB (`better-ccflare.db`) DOES exist:
/// 1. Creates a timestamped backup of the Node DB
/// 2. Copies `better-ccflare.db` → `better-ccflare-rs.db`
///
/// If the RS DB already exists, this is a no-op.
/// If neither exists, this is a no-op (fresh install).
pub fn migrate_from_node_db(db_path: &Path) -> Result<bool, DbError> {
    if db_path.exists() {
        return Ok(false);
    }

    let node_path = paths::resolve_node_db_path();
    if !node_path.exists() {
        return Ok(false);
    }

    tracing::info!(
        node_db = %node_path.display(),
        rs_db = %db_path.display(),
        "Node/TS database found — copying to RS location"
    );

    // Ensure target directory exists
    paths::ensure_db_dir(db_path)?;

    // Create a timestamped backup of the Node DB (safety net)
    backup_existing_db(&node_path)?;

    // Copy Node DB → RS DB
    std::fs::copy(&node_path, db_path)?;

    // Also copy WAL and SHM if present
    let node_wal = node_path.with_extension("db-wal");
    let node_shm = node_path.with_extension("db-shm");
    let target_wal = db_path.with_extension("db-wal");
    let target_shm = db_path.with_extension("db-shm");

    if node_wal.exists() {
        let _ = std::fs::copy(&node_wal, &target_wal);
    }
    if node_shm.exists() {
        let _ = std::fs::copy(&node_shm, &target_shm);
    }

    tracing::info!("Node database copy complete — RS version will auto-migrate schema");
    Ok(true)
}

// ---------------------------------------------------------------------------
// Schema migrations — upgrade TS-era database to RS schema
// ---------------------------------------------------------------------------

/// Get the column names of a table.
fn table_columns(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

/// Check if a table exists.
fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// Add a column to a table if it doesn't already exist.
fn add_column_if_missing(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    col_type: &str,
    existing_cols: &[String],
) {
    if existing_cols.iter().any(|c| c == column) {
        return;
    }
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}");
    if let Err(e) = conn.execute(&sql, []) {
        tracing::warn!("Failed to add column {table}.{column}: {e}");
    } else {
        tracing::info!("Added column {table}.{column}");
    }
}

/// Run schema migrations to upgrade a TS-era database to the RS schema.
///
/// This is idempotent — safe to run multiple times on the same database.
/// Called after `create_tables()` which handles brand-new tables via
/// `CREATE TABLE IF NOT EXISTS`.
///
/// Handles:
/// 1. Adding missing columns to accounts, requests, oauth_sessions
/// 2. Renaming `output_tokens_per_second` → `tokens_per_second` in requests
/// 3. Restructuring `request_payloads` from (id, json) → (request_id, request_body, response_body)
/// 4. Restructuring `agent_preferences` from (agent_id, model, updated_at) → expanded columns
pub fn run_schema_migrations(conn: &rusqlite::Connection) -> Result<(), DbError> {
    // Run all migrations inside a single transaction so a crash mid-migration
    // leaves the schema fully migrated or fully un-migrated (never partial).
    let tx = conn.unchecked_transaction()?;
    let result = run_schema_migrations_impl(&tx);
    match result {
        Ok(()) => {
            tx.commit()?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback();
            Err(e)
        }
    }
}

fn run_schema_migrations_impl(conn: &rusqlite::Connection) -> Result<(), DbError> {
    // -- accounts table --
    if table_exists(conn, "accounts") {
        let cols = table_columns(conn, "accounts");
        add_column_if_missing(conn, "accounts", "rate_limited_until", "INTEGER", &cols);
        add_column_if_missing(conn, "accounts", "session_start", "INTEGER", &cols);
        add_column_if_missing(
            conn,
            "accounts",
            "session_request_count",
            "INTEGER DEFAULT 0",
            &cols,
        );
        add_column_if_missing(conn, "accounts", "paused", "INTEGER DEFAULT 0", &cols);
        add_column_if_missing(conn, "accounts", "rate_limit_reset", "INTEGER", &cols);
        add_column_if_missing(conn, "accounts", "rate_limit_status", "TEXT", &cols);
        add_column_if_missing(conn, "accounts", "rate_limit_remaining", "INTEGER", &cols);
        add_column_if_missing(conn, "accounts", "priority", "INTEGER DEFAULT 0", &cols);
        add_column_if_missing(
            conn,
            "accounts",
            "auto_fallback_enabled",
            "INTEGER DEFAULT 1",
            &cols,
        );
        add_column_if_missing(
            conn,
            "accounts",
            "auto_refresh_enabled",
            "INTEGER DEFAULT 1",
            &cols,
        );
        add_column_if_missing(conn, "accounts", "custom_endpoint", "TEXT", &cols);
        add_column_if_missing(conn, "accounts", "model_mappings", "TEXT", &cols);
        // Rename reserve_percent → reserve_5h (if old column exists)
        if cols.iter().any(|c| c == "reserve_percent") && !cols.iter().any(|c| c == "reserve_5h") {
            if let Err(e) = conn.execute(
                "ALTER TABLE accounts RENAME COLUMN reserve_percent TO reserve_5h",
                [],
            ) {
                tracing::warn!("Failed to rename reserve_percent → reserve_5h: {e}");
                // Fallback: add as new column if rename not supported (old SQLite)
                add_column_if_missing(conn, "accounts", "reserve_5h", "INTEGER DEFAULT 0", &cols);
            } else {
                tracing::info!("Renamed accounts.reserve_percent → reserve_5h");
            }
        } else {
            add_column_if_missing(
                conn,
                "accounts",
                "reserve_5h",
                "INTEGER DEFAULT 0",
                &cols,
            );
        }
        // Re-read cols after potential rename
        let cols = table_columns(conn, "accounts");
        add_column_if_missing(
            conn,
            "accounts",
            "reserve_weekly",
            "INTEGER DEFAULT 0",
            &cols,
        );
        add_column_if_missing(
            conn,
            "accounts",
            "reserve_hard",
            "INTEGER DEFAULT 0",
            &cols,
        );
        add_column_if_missing(conn, "accounts", "subscription_tier", "TEXT", &cols);
        add_column_if_missing(conn, "accounts", "email", "TEXT", &cols);
        add_column_if_missing(
            conn,
            "accounts",
            "monthly_cost_usd",
            "REAL NOT NULL DEFAULT 0",
            &cols,
        );
        add_column_if_missing(
            conn,
            "accounts",
            "refresh_token_updated_at",
            "INTEGER",
            &cols,
        );
        // Backfill refresh_token_updated_at with created_at for existing accounts.
        // This is a conservative baseline — will be updated to the actual re-auth
        // time on next successful token refresh.
        let _ = conn.execute(
            "UPDATE accounts SET refresh_token_updated_at = created_at WHERE refresh_token_updated_at IS NULL AND created_at != 0",
            [],
        );
        // Auto-detect monthly subscription cost from account name multiplier suffix.
        // Patterns: "- 20x" → $200/mo (Claude Max 20x), "- 6.5x" → $130/mo,
        //           "- 5x" → $100/mo (Claude Max 5x), OAuth default → $20/mo (Pro).
        // Only updates rows that are still at the default (0) to preserve user edits.
        let _ = conn.execute(
            "UPDATE accounts SET monthly_cost_usd = CASE
                WHEN name LIKE '% - 20x' OR name LIKE '% 20x' THEN 200.0
                WHEN name LIKE '% - 6.5x' OR name LIKE '% 6.5x' THEN 130.0
                WHEN name LIKE '% - 5x'  OR name LIKE '% 5x'  THEN 100.0
                WHEN provider IN ('anthropic', 'claude-oauth', 'console') THEN 20.0
                ELSE 0.0
             END
             WHERE monthly_cost_usd = 0",
            [],
        );

        // Make name UNIQUE if not already (TS schema didn't enforce this)
        // We can't ALTER an existing constraint, so we just ignore duplicates
    }

    // -- requests table --
    if table_exists(conn, "requests") {
        let cols = table_columns(conn, "requests");
        add_column_if_missing(conn, "requests", "model", "TEXT", &cols);
        add_column_if_missing(
            conn,
            "requests",
            "prompt_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(
            conn,
            "requests",
            "completion_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(
            conn,
            "requests",
            "total_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(conn, "requests", "cost_usd", "REAL", &cols);
        add_column_if_missing(
            conn,
            "requests",
            "input_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(
            conn,
            "requests",
            "cache_read_input_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(
            conn,
            "requests",
            "cache_creation_input_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(
            conn,
            "requests",
            "output_tokens",
            "INTEGER",
            &cols,
        );
        add_column_if_missing(conn, "requests", "agent_used", "TEXT", &cols);
        add_column_if_missing(conn, "requests", "project", "TEXT", &cols);
        add_column_if_missing(conn, "requests", "api_key_id", "TEXT", &cols);
        add_column_if_missing(conn, "requests", "api_key_name", "TEXT", &cols);

        // Handle output_tokens_per_second → tokens_per_second rename
        if cols.iter().any(|c| c == "output_tokens_per_second")
            && !cols.iter().any(|c| c == "tokens_per_second")
        {
            conn.execute(
                "ALTER TABLE requests ADD COLUMN tokens_per_second REAL",
                [],
            )?;
            conn.execute(
                "UPDATE requests SET tokens_per_second = output_tokens_per_second WHERE output_tokens_per_second IS NOT NULL",
                [],
            )?;
            tracing::info!("Migrated output_tokens_per_second → tokens_per_second");
        } else {
            add_column_if_missing(conn, "requests", "tokens_per_second", "REAL", &cols);
        }
    }

    // -- oauth_sessions table --
    if table_exists(conn, "oauth_sessions") {
        let cols = table_columns(conn, "oauth_sessions");
        add_column_if_missing(conn, "oauth_sessions", "custom_endpoint", "TEXT", &cols);
    }

    // -- request_payloads table restructuring --
    // TS schema: (id TEXT PK, json TEXT NOT NULL)
    // RS schema: (request_id TEXT PK, request_body TEXT, response_body TEXT)
    if table_exists(conn, "request_payloads") {
        let cols = table_columns(conn, "request_payloads");
        let has_old_schema = cols.iter().any(|c| c == "json") && !cols.iter().any(|c| c == "request_body");

        if has_old_schema {
            tracing::info!("Restructuring request_payloads table (old → new schema)");
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS request_payloads_new (
                    request_id TEXT PRIMARY KEY,
                    request_body TEXT,
                    response_body TEXT,
                    FOREIGN KEY (request_id) REFERENCES requests(id)
                );

                INSERT OR IGNORE INTO request_payloads_new (request_id, request_body, response_body)
                SELECT id, json, NULL FROM request_payloads;

                DROP TABLE request_payloads;
                ALTER TABLE request_payloads_new RENAME TO request_payloads;
                ",
            )?;
            tracing::info!("request_payloads restructured successfully");
        }
    }

    // -- agent_preferences table restructuring --
    // TS schema: (agent_id TEXT PK, model TEXT NOT NULL, updated_at INTEGER NOT NULL)
    // RS schema: (agent_id TEXT PK, preferred_account TEXT, preferred_model TEXT,
    //             max_tokens INTEGER, temperature REAL, system_prompt TEXT,
    //             created_at INTEGER, updated_at INTEGER)
    if table_exists(conn, "agent_preferences") {
        let cols = table_columns(conn, "agent_preferences");
        let has_old_schema = cols.iter().any(|c| c == "model") && !cols.iter().any(|c| c == "preferred_model");

        if has_old_schema {
            tracing::info!("Restructuring agent_preferences table (old → new schema)");
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS agent_preferences_new (
                    agent_id TEXT PRIMARY KEY,
                    preferred_account TEXT,
                    preferred_model TEXT,
                    max_tokens INTEGER,
                    temperature REAL,
                    system_prompt TEXT,
                    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000),
                    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000)
                );

                INSERT OR IGNORE INTO agent_preferences_new (agent_id, preferred_model, updated_at, created_at)
                SELECT agent_id, model, updated_at, updated_at FROM agent_preferences;

                DROP TABLE agent_preferences;
                ALTER TABLE agent_preferences_new RENAME TO agent_preferences;
                ",
            )?;
            tracing::info!("agent_preferences restructured successfully");
        }
    }

    Ok(())
}

/// Check whether the database has been initialized (has the accounts table).
pub fn is_initialized(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='accounts'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn is_initialized_empty_db() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(!is_initialized(&conn));
    }

    #[test]
    fn is_initialized_with_tables() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::create_tables(&conn).unwrap();
        assert!(is_initialized(&conn));
    }

    #[test]
    fn backup_nonexistent_db() {
        let path = std::path::PathBuf::from("/tmp/nonexistent_test_db.db");
        assert!(backup_existing_db(&path).unwrap().is_none());
    }

    #[test]
    fn backup_existing_db_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        fs::write(&db_path, b"test data").unwrap();

        let backup = backup_existing_db(&db_path).unwrap();
        assert!(backup.is_some());
        let backup_path = backup.unwrap();
        assert!(backup_path.exists());
        assert!(backup_path.to_string_lossy().contains(".bak."));
    }

    #[test]
    fn migrate_from_legacy_no_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("better-ccflare.db");
        // No legacy DB exists, so migration should return false
        let result = migrate_from_legacy(&db_path).unwrap();
        assert!(!result);
    }

    #[test]
    fn migrate_from_legacy_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("better-ccflare.db");
        fs::write(&db_path, b"existing").unwrap();
        // DB already exists, so migration should return false
        let result = migrate_from_legacy(&db_path).unwrap();
        assert!(!result);
    }

    /// Simulate a TS-era database schema, run migrations, verify all columns and
    /// table restructures are applied correctly.
    #[test]
    fn migrate_old_ts_schema_to_rust() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();

        // Create TS-era tables (minimal columns, old structure)
        conn.execute_batch(
            "
            CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                provider TEXT NOT NULL DEFAULT 'anthropic',
                api_key TEXT,
                refresh_token TEXT NOT NULL DEFAULT '',
                access_token TEXT,
                expires_at INTEGER,
                request_count INTEGER DEFAULT 0,
                total_requests INTEGER DEFAULT 0,
                last_used INTEGER,
                created_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE requests (
                id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                account_used TEXT,
                status_code INTEGER,
                success INTEGER DEFAULT 0,
                error_message TEXT,
                response_time_ms INTEGER,
                failover_attempts INTEGER DEFAULT 0,
                output_tokens_per_second REAL
            );

            CREATE TABLE request_payloads (
                id TEXT PRIMARY KEY,
                json TEXT NOT NULL
            );

            CREATE TABLE agent_preferences (
                agent_id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE oauth_sessions (
                id TEXT PRIMARY KEY,
                account_name TEXT NOT NULL,
                verifier TEXT NOT NULL,
                mode TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            ",
        )
        .unwrap();

        // Insert test data
        conn.execute(
            "INSERT INTO accounts (id, name) VALUES ('a1', 'test-acct')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO requests (id, timestamp, method, path, output_tokens_per_second) VALUES ('r1', 1000, 'POST', '/v1/messages', 42.5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO request_payloads (id, json) VALUES ('r1', '{\"body\":\"test\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agent_preferences (agent_id, model, updated_at) VALUES ('claude-code', 'claude-sonnet-4-5-20250929', 1000)",
            [],
        )
        .unwrap();

        // Run migrations
        run_schema_migrations(&conn).unwrap();

        // Verify accounts columns added
        let acc_cols = table_columns(&conn, "accounts");
        for col in &[
            "rate_limited_until",
            "session_start",
            "session_request_count",
            "paused",
            "rate_limit_reset",
            "rate_limit_status",
            "rate_limit_remaining",
            "priority",
            "auto_fallback_enabled",
            "auto_refresh_enabled",
            "custom_endpoint",
            "model_mappings",
        ] {
            assert!(
                acc_cols.iter().any(|c| c == col),
                "accounts missing column: {col}"
            );
        }

        // Verify requests columns added and rename
        let req_cols = table_columns(&conn, "requests");
        for col in &[
            "model",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "cost_usd",
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "output_tokens",
            "agent_used",
            "project",
            "api_key_id",
            "api_key_name",
            "tokens_per_second",
        ] {
            assert!(
                req_cols.iter().any(|c| c == col),
                "requests missing column: {col}"
            );
        }

        // Verify output_tokens_per_second data was copied to tokens_per_second
        let tps: f64 = conn
            .query_row(
                "SELECT tokens_per_second FROM requests WHERE id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((tps - 42.5).abs() < 0.01);

        // Verify request_payloads restructured
        let payload_cols = table_columns(&conn, "request_payloads");
        assert!(payload_cols.iter().any(|c| c == "request_id"));
        assert!(payload_cols.iter().any(|c| c == "request_body"));
        assert!(payload_cols.iter().any(|c| c == "response_body"));
        assert!(!payload_cols.iter().any(|c| c == "json"));

        // Verify data migrated
        let body: String = conn
            .query_row(
                "SELECT request_body FROM request_payloads WHERE request_id = 'r1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(body, "{\"body\":\"test\"}");

        // Verify agent_preferences restructured
        let agent_cols = table_columns(&conn, "agent_preferences");
        assert!(agent_cols.iter().any(|c| c == "preferred_model"));
        assert!(!agent_cols.iter().any(|c| c == "model"));

        // Verify data migrated
        let pref_model: String = conn
            .query_row(
                "SELECT preferred_model FROM agent_preferences WHERE agent_id = 'claude-code'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pref_model, "claude-sonnet-4-5-20250929");

        // Verify oauth_sessions got custom_endpoint
        let oauth_cols = table_columns(&conn, "oauth_sessions");
        assert!(oauth_cols.iter().any(|c| c == "custom_endpoint"));
    }

    /// Run migrations twice — should be idempotent with no errors.
    #[test]
    fn migration_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();

        // Create TS-era tables
        conn.execute_batch(
            "
            CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                provider TEXT NOT NULL DEFAULT 'anthropic'
            );
            CREATE TABLE requests (
                id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                output_tokens_per_second REAL
            );
            CREATE TABLE request_payloads (
                id TEXT PRIMARY KEY,
                json TEXT NOT NULL
            );
            CREATE TABLE agent_preferences (
                agent_id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE oauth_sessions (
                id TEXT PRIMARY KEY,
                account_name TEXT NOT NULL,
                verifier TEXT NOT NULL,
                mode TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            ",
        )
        .unwrap();

        // First run
        run_schema_migrations(&conn).unwrap();
        // Second run — should not error
        run_schema_migrations(&conn).unwrap();

        // Verify tables are intact
        assert!(table_exists(&conn, "accounts"));
        assert!(table_exists(&conn, "requests"));
        assert!(table_exists(&conn, "request_payloads"));
        assert!(table_exists(&conn, "agent_preferences"));
    }

    /// Run migrations on a fresh RS schema (create_tables first) — should be a no-op.
    #[test]
    fn migration_on_fresh_rs_schema() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::create_tables(&conn).unwrap();

        // Running migrations on a fresh RS database should be a no-op
        run_schema_migrations(&conn).unwrap();

        // Verify tables are intact with correct columns
        let cols = table_columns(&conn, "request_payloads");
        assert!(cols.iter().any(|c| c == "request_body"));
        assert!(!cols.iter().any(|c| c == "json"));
    }
}
