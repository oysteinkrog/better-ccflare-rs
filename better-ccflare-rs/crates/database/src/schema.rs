//! Schema creation — all tables matching the TypeScript schema.
//!
//! Creates tables and performance indexes in a single transaction.

use rusqlite::Connection;

use crate::error::DbError;

/// Create all tables if they don't exist.
pub fn create_tables(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}

/// Create performance indexes.
pub fn create_indexes(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(INDEXES_SQL)?;
    Ok(())
}

/// Full schema DDL — matches the TypeScript migrations exactly.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    provider TEXT NOT NULL DEFAULT 'anthropic',
    api_key TEXT,
    refresh_token TEXT NOT NULL DEFAULT '',
    access_token TEXT,
    expires_at INTEGER,
    request_count INTEGER DEFAULT 0,
    total_requests INTEGER DEFAULT 0,
    last_used INTEGER,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000),
    rate_limited_until INTEGER,
    session_start INTEGER,
    session_request_count INTEGER DEFAULT 0,
    paused INTEGER DEFAULT 0,
    rate_limit_reset INTEGER,
    rate_limit_status TEXT,
    rate_limit_remaining INTEGER,
    priority INTEGER DEFAULT 0,
    auto_fallback_enabled INTEGER DEFAULT 1,
    auto_refresh_enabled INTEGER DEFAULT 1,
    custom_endpoint TEXT,
    model_mappings TEXT,
    reserve_5h INTEGER DEFAULT 0,
    reserve_weekly INTEGER DEFAULT 0,
    reserve_hard INTEGER DEFAULT 0,
    subscription_tier TEXT,
    email TEXT
);

CREATE TABLE IF NOT EXISTS requests (
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
    model TEXT,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    cost_usd REAL,
    input_tokens INTEGER,
    cache_read_input_tokens INTEGER,
    cache_creation_input_tokens INTEGER,
    output_tokens INTEGER,
    agent_used TEXT,
    tokens_per_second REAL,
    project TEXT,
    api_key_id TEXT,
    api_key_name TEXT
);

CREATE TABLE IF NOT EXISTS request_payloads (
    request_id TEXT PRIMARY KEY,
    request_body TEXT,
    response_body TEXT,
    FOREIGN KEY (request_id) REFERENCES requests(id)
);

CREATE TABLE IF NOT EXISTS oauth_sessions (
    id TEXT PRIMARY KEY,
    account_name TEXT NOT NULL,
    verifier TEXT NOT NULL,
    mode TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    custom_endpoint TEXT
);

CREATE TABLE IF NOT EXISTS agent_preferences (
    agent_id TEXT PRIMARY KEY,
    preferred_account TEXT,
    preferred_model TEXT,
    max_tokens INTEGER,
    temperature REAL,
    system_prompt TEXT,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000),
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000)
);

CREATE TABLE IF NOT EXISTS api_keys (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    hashed_key TEXT NOT NULL UNIQUE,
    prefix_last_8 TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000),
    last_used INTEGER,
    usage_count INTEGER DEFAULT 0,
    is_active INTEGER DEFAULT 1
);

CREATE TABLE IF NOT EXISTS strategies (
    name TEXT PRIMARY KEY,
    config TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now') * 1000)
);
"#;

/// Performance indexes — matches the TypeScript performance-indexes.ts.
const INDEXES_SQL: &str = r#"
-- Request indexes
CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON requests(timestamp);
CREATE INDEX IF NOT EXISTS idx_requests_account_used ON requests(account_used);
CREATE INDEX IF NOT EXISTS idx_requests_success ON requests(success);
CREATE INDEX IF NOT EXISTS idx_requests_model ON requests(model);
CREATE INDEX IF NOT EXISTS idx_requests_status_code ON requests(status_code);
CREATE INDEX IF NOT EXISTS idx_requests_timestamp_account ON requests(timestamp, account_used);
CREATE INDEX IF NOT EXISTS idx_requests_timestamp_success ON requests(timestamp, success);
CREATE INDEX IF NOT EXISTS idx_requests_timestamp_model ON requests(timestamp, model);
CREATE INDEX IF NOT EXISTS idx_requests_account_timestamp ON requests(account_used, timestamp);
CREATE INDEX IF NOT EXISTS idx_requests_agent_used ON requests(agent_used);
CREATE INDEX IF NOT EXISTS idx_requests_project ON requests(project);
CREATE INDEX IF NOT EXISTS idx_requests_api_key_id ON requests(api_key_id);
CREATE INDEX IF NOT EXISTS idx_requests_cost ON requests(cost_usd) WHERE cost_usd IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_requests_tokens ON requests(total_tokens) WHERE total_tokens IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_requests_response_time ON requests(response_time_ms) WHERE response_time_ms IS NOT NULL;

-- Account indexes
CREATE INDEX IF NOT EXISTS idx_accounts_provider ON accounts(provider);
CREATE INDEX IF NOT EXISTS idx_accounts_paused ON accounts(paused);
CREATE INDEX IF NOT EXISTS idx_accounts_priority ON accounts(priority);
CREATE INDEX IF NOT EXISTS idx_accounts_rate_limited ON accounts(rate_limited_until) WHERE rate_limited_until IS NOT NULL;

-- OAuth session indexes
CREATE INDEX IF NOT EXISTS idx_oauth_sessions_expires ON oauth_sessions(expires_at);

-- API key indexes
CREATE INDEX IF NOT EXISTS idx_api_keys_active ON api_keys(is_active);
CREATE INDEX IF NOT EXISTS idx_api_keys_hashed ON api_keys(hashed_key);

-- Agent preference indexes
CREATE INDEX IF NOT EXISTS idx_agent_preferences_agent ON agent_preferences(agent_id);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"accounts".to_string()));
        assert!(tables.contains(&"requests".to_string()));
        assert!(tables.contains(&"request_payloads".to_string()));
        assert!(tables.contains(&"oauth_sessions".to_string()));
        assert!(tables.contains(&"agent_preferences".to_string()));
        assert!(tables.contains(&"api_keys".to_string()));
        assert!(tables.contains(&"strategies".to_string()));
    }

    #[test]
    fn indexes_created_successfully() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        create_indexes(&conn).unwrap();

        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // We have 23 indexes defined
        assert!(
            index_count >= 20,
            "Expected at least 20 indexes, got {index_count}"
        );
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        create_indexes(&conn).unwrap();
        // Running again should not error
        create_tables(&conn).unwrap();
        create_indexes(&conn).unwrap();
    }

    #[test]
    fn accounts_table_has_expected_columns() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(accounts)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let expected = [
            "id",
            "name",
            "provider",
            "api_key",
            "refresh_token",
            "access_token",
            "expires_at",
            "request_count",
            "total_requests",
            "last_used",
            "created_at",
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
            "reserve_5h",
            "reserve_weekly",
            "reserve_hard",
            "subscription_tier",
            "email",
        ];

        for col in &expected {
            assert!(cols.contains(&col.to_string()), "Missing column: {col}");
        }
    }
}
