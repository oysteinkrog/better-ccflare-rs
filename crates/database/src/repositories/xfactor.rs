//! X-factor state repository — persist and restore Bayesian posterior state.

use rusqlite::{params, Connection};

use crate::error::DbError;

// ---------------------------------------------------------------------------
// Persisted state type
// ---------------------------------------------------------------------------

/// Persisted X-factor estimator state for one account.
#[derive(Debug, Clone)]
pub struct XFactorDbState {
    pub account_id: String,
    pub mu: f64,
    pub sigma_sq: f64,
    pub n_eff: f64,
    pub ema_proxy_rate: f64,
    pub c_i_hard_lower: f64,
    pub updated_at_ms: i64,
    // Phase 3: Kalman Filter state for external usage estimation
    pub kf_e: f64,
    pub kf_e_dot: f64,
    pub kf_p00: f64,
    pub kf_p01: f64,
    pub kf_p10: f64,
    pub kf_p11: f64,
    pub lag_estimate_ms: i64,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// Upsert the estimator state for one account.
pub fn save_state(conn: &Connection, state: &XFactorDbState) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO account_xfactor_state
            (account_id, mu, sigma_sq, n_eff, ema_proxy_rate, c_i_hard_lower, updated_at,
             kf_e, kf_e_dot, kf_p00, kf_p01, kf_p10, kf_p11, lag_estimate_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(account_id) DO UPDATE SET
            mu = excluded.mu,
            sigma_sq = excluded.sigma_sq,
            n_eff = excluded.n_eff,
            ema_proxy_rate = excluded.ema_proxy_rate,
            c_i_hard_lower = excluded.c_i_hard_lower,
            updated_at = excluded.updated_at,
            kf_e = excluded.kf_e,
            kf_e_dot = excluded.kf_e_dot,
            kf_p00 = excluded.kf_p00,
            kf_p01 = excluded.kf_p01,
            kf_p10 = excluded.kf_p10,
            kf_p11 = excluded.kf_p11,
            lag_estimate_ms = excluded.lag_estimate_ms",
        params![
            state.account_id,
            state.mu,
            state.sigma_sq,
            state.n_eff,
            state.ema_proxy_rate,
            state.c_i_hard_lower,
            state.updated_at_ms,
            state.kf_e,
            state.kf_e_dot,
            state.kf_p00,
            state.kf_p01,
            state.kf_p10,
            state.kf_p11,
            state.lag_estimate_ms,
        ],
    )?;
    Ok(())
}

/// Load persisted state for a single account (returns None if not yet persisted).
pub fn load_state(conn: &Connection, account_id: &str) -> Result<Option<XFactorDbState>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, mu, sigma_sq, n_eff, ema_proxy_rate, c_i_hard_lower, updated_at,
                kf_e, kf_e_dot, kf_p00, kf_p01, kf_p10, kf_p11, lag_estimate_ms
         FROM account_xfactor_state WHERE account_id = ?1",
    )?;
    let mut rows = stmt.query(params![account_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(XFactorDbState {
            account_id: row.get(0)?,
            mu: row.get(1)?,
            sigma_sq: row.get(2)?,
            n_eff: row.get(3)?,
            ema_proxy_rate: row.get(4)?,
            c_i_hard_lower: row.get(5)?,
            updated_at_ms: row.get(6)?,
            kf_e: row.get(7).unwrap_or(0.0),
            kf_e_dot: row.get(8).unwrap_or(0.0),
            kf_p00: row.get(9).unwrap_or(1e9),
            kf_p01: row.get(10).unwrap_or(0.0),
            kf_p10: row.get(11).unwrap_or(0.0),
            kf_p11: row.get(12).unwrap_or(1e9),
            lag_estimate_ms: row.get(13).unwrap_or(90_000),
        }))
    } else {
        Ok(None)
    }
}

/// Load all persisted states.
pub fn load_all_states(conn: &Connection) -> Result<Vec<XFactorDbState>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, mu, sigma_sq, n_eff, ema_proxy_rate, c_i_hard_lower, updated_at,
                kf_e, kf_e_dot, kf_p00, kf_p01, kf_p10, kf_p11, lag_estimate_ms
         FROM account_xfactor_state",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(XFactorDbState {
            account_id: row.get(0)?,
            mu: row.get(1)?,
            sigma_sq: row.get(2)?,
            n_eff: row.get(3)?,
            ema_proxy_rate: row.get(4)?,
            c_i_hard_lower: row.get(5)?,
            updated_at_ms: row.get(6)?,
            kf_e: row.get(7).unwrap_or(0.0),
            kf_e_dot: row.get(8).unwrap_or(0.0),
            kf_p00: row.get(9).unwrap_or(1e9),
            kf_p01: row.get(10).unwrap_or(0.0),
            kf_p10: row.get(11).unwrap_or(0.0),
            kf_p11: row.get(12).unwrap_or(1e9),
            lag_estimate_ms: row.get(13).unwrap_or(90_000),
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Save a batch of states in a single transaction (for periodic persistence).
pub fn save_all_states(conn: &Connection, states: &[XFactorDbState]) -> Result<(), DbError> {
    if states.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for state in states {
        save_state(&tx, state)?;
    }
    tx.commit()?;
    Ok(())
}

/// Query the weighted token sum for an account over the last 5h from the
/// `requests` table. Used to rebuild the rolling window on startup.
///
/// Returns a vec of (timestamp_ms, weighted_tokens) pairs.
pub fn recent_requests_for_account(
    conn: &Connection,
    account_id: &str,
    since_ms: i64,
) -> Result<Vec<(i64, f64, Option<String>)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT timestamp,
                COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)
                    + COALESCE(cache_creation_input_tokens, 0)
                    + COALESCE(cache_read_input_tokens, 0),
                model
         FROM requests
         WHERE account_used = ?1 AND timestamp >= ?2
         ORDER BY timestamp ASC",
    )?;
    let rows = stmt.query_map(params![account_id, since_ms], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Query the monthly_cost_usd for an account directly from DB.
pub fn get_monthly_cost_usd(conn: &Connection, account_id: &str) -> Result<f64, DbError> {
    let val: f64 = conn.query_row(
        "SELECT COALESCE(monthly_cost_usd, 0) FROM accounts WHERE id = ?1",
        params![account_id],
        |row| row.get(0),
    )?;
    Ok(val)
}

/// Query rolling 30-day token and cost aggregates per account.
///
/// Returns (account_id, raw_tokens, payg_cost_usd, request_count, first_ts_ms, last_ts_ms).
pub fn value_aggregates(conn: &Connection, since_ms: i64) -> Result<Vec<ValueAggregate>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT account_used,
                SUM(COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0)
                    + COALESCE(cache_creation_input_tokens, 0)
                    + COALESCE(cache_read_input_tokens, 0)) AS raw_tokens,
                SUM(
                    COALESCE(output_tokens, 0) * 15.0 / 1e6
                    + COALESCE(input_tokens, 0) * 3.0 / 1e6
                    + COALESCE(cache_creation_input_tokens, 0) * 3.75 / 1e6
                    + COALESCE(cache_read_input_tokens, 0) * 0.30 / 1e6
                ) AS payg_cost_usd,
                COUNT(*) AS request_count,
                MIN(timestamp) AS first_ts,
                MAX(timestamp) AS last_ts
         FROM requests
         WHERE timestamp >= ?1 AND account_used IS NOT NULL AND success = 1
         GROUP BY account_used",
    )?;
    let rows = stmt.query_map(params![since_ms], |row| {
        Ok(ValueAggregate {
            account_id: row.get(0)?,
            raw_tokens: row.get::<_, f64>(1).unwrap_or(0.0),
            payg_cost_usd: row.get::<_, f64>(2).unwrap_or(0.0),
            request_count: row.get::<_, i64>(3).unwrap_or(0),
            first_ts_ms: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            last_ts_ms: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Rate-limit counts for the last 7 days per account.
pub fn rate_limit_counts_7d(
    conn: &Connection,
    since_ms: i64,
) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT account_used, COUNT(*) AS cnt
         FROM requests
         WHERE timestamp >= ?1 AND status_code = 429 AND account_used IS NOT NULL
         GROUP BY account_used",
    )?;
    let rows = stmt.query_map(params![since_ms], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Aggregate per-account value data from the requests table.
#[derive(Debug, Clone)]
pub struct ValueAggregate {
    pub account_id: String,
    pub raw_tokens: f64,
    pub payg_cost_usd: f64,
    pub request_count: i64,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                monthly_cost_usd REAL DEFAULT 0
            );
            CREATE TABLE account_xfactor_state (
                account_id TEXT PRIMARY KEY,
                mu REAL NOT NULL,
                sigma_sq REAL NOT NULL,
                n_eff REAL NOT NULL DEFAULT 0,
                ema_proxy_rate REAL NOT NULL DEFAULT 0,
                c_i_hard_lower REAL NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                kf_e REAL NOT NULL DEFAULT 0,
                kf_e_dot REAL NOT NULL DEFAULT 0,
                kf_p00 REAL NOT NULL DEFAULT 1000000000,
                kf_p01 REAL NOT NULL DEFAULT 0,
                kf_p10 REAL NOT NULL DEFAULT 0,
                kf_p11 REAL NOT NULL DEFAULT 1000000000,
                lag_estimate_ms INTEGER NOT NULL DEFAULT 90000
            );
            CREATE TABLE requests (
                id TEXT PRIMARY KEY,
                timestamp INTEGER,
                account_used TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_creation_input_tokens INTEGER,
                cache_read_input_tokens INTEGER,
                model TEXT,
                status_code INTEGER,
                success INTEGER DEFAULT 1
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn save_and_load_state() {
        let conn = in_memory_db();
        // Insert a parent account first
        conn.execute("INSERT INTO accounts (id) VALUES ('acc1')", [])
            .unwrap();

        let state = XFactorDbState {
            account_id: "acc1".to_string(),
            mu: 11.385,
            sigma_sq: 0.125,
            n_eff: 5.0,
            ema_proxy_rate: 10.0,
            c_i_hard_lower: 50_000.0,
            updated_at_ms: 1_700_000_000_000,
            kf_e: 1000.0,
            kf_e_dot: 2.5,
            kf_p00: 5_000_000.0,
            kf_p01: 0.0,
            kf_p10: 0.0,
            kf_p11: 100_000.0,
            lag_estimate_ms: 90_000,
        };

        save_state(&conn, &state).unwrap();
        let loaded = load_state(&conn, "acc1").unwrap().unwrap();
        assert!((loaded.mu - 11.385).abs() < 1e-9);
        assert!((loaded.sigma_sq - 0.125).abs() < 1e-9);
        assert_eq!(loaded.n_eff, 5.0);
    }

    #[test]
    fn load_state_missing_returns_none() {
        let conn = in_memory_db();
        assert!(load_state(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn upsert_updates_existing() {
        let conn = in_memory_db();
        conn.execute("INSERT INTO accounts (id) VALUES ('acc1')", [])
            .unwrap();

        let state1 = XFactorDbState {
            account_id: "acc1".to_string(),
            mu: 10.0,
            sigma_sq: 0.5,
            n_eff: 2.0,
            ema_proxy_rate: 5.0,
            c_i_hard_lower: 0.0,
            updated_at_ms: 1000,
            kf_e: 0.0,
            kf_e_dot: 0.0,
            kf_p00: 1e9,
            kf_p01: 0.0,
            kf_p10: 0.0,
            kf_p11: 1e9,
            lag_estimate_ms: 90_000,
        };
        save_state(&conn, &state1).unwrap();

        let state2 = XFactorDbState {
            mu: 11.0,
            updated_at_ms: 2000,
            ..state1
        };
        save_state(&conn, &state2).unwrap();

        let loaded = load_state(&conn, "acc1").unwrap().unwrap();
        assert!((loaded.mu - 11.0).abs() < 1e-9);
        assert_eq!(loaded.updated_at_ms, 2000);
    }

    #[test]
    fn value_aggregates_empty() {
        let conn = in_memory_db();
        let result = value_aggregates(&conn, 0).unwrap();
        assert!(result.is_empty());
    }
}
