//! Account repository — CRUD operations for the `accounts` table.
//!
//! All SQL queries match the TypeScript `AccountRepository` exactly.

use rusqlite::{params, Connection, OptionalExtension};

use bccf_core::types::Account;

use crate::error::DbError;

/// The full SELECT column list with COALESCE for boolean fields.
const ACCOUNT_SELECT: &str = "
    SELECT
        id, name, provider, api_key, refresh_token, access_token,
        expires_at, created_at, last_used, request_count, total_requests,
        rate_limited_until, session_start, session_request_count,
        COALESCE(paused, 0) as paused,
        rate_limit_reset, rate_limit_status, rate_limit_remaining,
        COALESCE(priority, 0) as priority,
        COALESCE(auto_fallback_enabled, 0) as auto_fallback_enabled,
        COALESCE(auto_refresh_enabled, 0) as auto_refresh_enabled,
        custom_endpoint,
        model_mappings,
        COALESCE(reserve_5h, 0) as reserve_5h,
        COALESCE(reserve_weekly, 0) as reserve_weekly,
        COALESCE(reserve_hard, 0) as reserve_hard
    FROM accounts
";

/// Map a rusqlite row to an Account struct.
fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get("id")?,
        name: row.get("name")?,
        provider: row.get("provider")?,
        api_key: row.get("api_key")?,
        refresh_token: row
            .get::<_, Option<String>>("refresh_token")?
            .unwrap_or_default(),
        access_token: row.get("access_token")?,
        expires_at: row.get("expires_at")?,
        request_count: row.get::<_, Option<i64>>("request_count")?.unwrap_or(0),
        total_requests: row.get::<_, Option<i64>>("total_requests")?.unwrap_or(0),
        last_used: row.get("last_used")?,
        created_at: row.get("created_at")?,
        rate_limited_until: row.get("rate_limited_until")?,
        session_start: row.get("session_start")?,
        session_request_count: row
            .get::<_, Option<i64>>("session_request_count")?
            .unwrap_or(0),
        paused: row.get::<_, i64>("paused")? != 0,
        rate_limit_reset: row.get("rate_limit_reset")?,
        rate_limit_status: row.get("rate_limit_status")?,
        rate_limit_remaining: row.get("rate_limit_remaining")?,
        priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(0),
        auto_fallback_enabled: row.get::<_, i64>("auto_fallback_enabled")? != 0,
        auto_refresh_enabled: row.get::<_, i64>("auto_refresh_enabled")? != 0,
        custom_endpoint: row.get("custom_endpoint")?,
        model_mappings: row.get("model_mappings")?,
        reserve_5h: row.get::<_, Option<i64>>("reserve_5h")?.unwrap_or(0),
        reserve_weekly: row.get::<_, Option<i64>>("reserve_weekly")?.unwrap_or(0),
        reserve_hard: row.get::<_, i64>("reserve_hard")? != 0,
    })
}

/// Fetch all accounts ordered by priority descending.
pub fn find_all(conn: &Connection) -> Result<Vec<Account>, DbError> {
    let sql = format!("{ACCOUNT_SELECT} ORDER BY priority DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_account)?;
    let mut accounts = Vec::new();
    for row in rows {
        accounts.push(row?);
    }
    Ok(accounts)
}

/// Fetch a single account by ID.
pub fn find_by_id(conn: &Connection, account_id: &str) -> Result<Option<Account>, DbError> {
    let sql = format!("{ACCOUNT_SELECT} WHERE id = ?1");
    let result = conn
        .query_row(&sql, params![account_id], row_to_account)
        .optional()?;
    Ok(result)
}

/// Fetch a single account by name.
pub fn find_by_name(conn: &Connection, name: &str) -> Result<Option<Account>, DbError> {
    let sql = format!("{ACCOUNT_SELECT} WHERE name = ?1");
    let result = conn
        .query_row(&sql, params![name], row_to_account)
        .optional()?;
    Ok(result)
}

/// Update access/refresh tokens for an account.
pub fn update_tokens(
    conn: &Connection,
    account_id: &str,
    access_token: &str,
    expires_at: i64,
    refresh_token: Option<&str>,
) -> Result<(), DbError> {
    if let Some(rt) = refresh_token {
        conn.execute(
            "UPDATE accounts SET access_token = ?1, expires_at = ?2, refresh_token = ?3 WHERE id = ?4",
            params![access_token, expires_at, rt, account_id],
        )?;
    } else {
        conn.execute(
            "UPDATE accounts SET access_token = ?1, expires_at = ?2 WHERE id = ?3",
            params![access_token, expires_at, account_id],
        )?;
    }
    Ok(())
}

/// Increment usage counters and manage session window.
pub fn increment_usage(
    conn: &Connection,
    account_id: &str,
    now: i64,
    session_duration_ms: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts
         SET
             last_used = ?1,
             request_count = COALESCE(request_count, 0) + 1,
             total_requests = COALESCE(total_requests, 0) + 1,
             session_start = CASE
                 WHEN session_start IS NULL OR ?1 - COALESCE(session_start, 0) >= ?2 THEN ?1
                 ELSE session_start
             END,
             session_request_count = CASE
                 WHEN session_start IS NULL OR ?1 - COALESCE(session_start, 0) >= ?2 THEN 1
                 ELSE COALESCE(session_request_count, 0) + 1
             END
         WHERE id = ?3",
        params![now, session_duration_ms, account_id],
    )?;
    Ok(())
}

/// Mark an account as rate-limited until a given timestamp.
pub fn set_rate_limited(conn: &Connection, account_id: &str, until: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET rate_limited_until = ?1 WHERE id = ?2",
        params![until, account_id],
    )?;
    Ok(())
}

/// Update rate limit metadata from response headers.
pub fn update_rate_limit_meta(
    conn: &Connection,
    account_id: &str,
    status: &str,
    reset: Option<i64>,
    remaining: Option<i64>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET rate_limit_status = ?1, rate_limit_reset = ?2, rate_limit_remaining = ?3 WHERE id = ?4",
        params![status, reset, remaining, account_id],
    )?;
    Ok(())
}

/// Pause an account.
pub fn pause(conn: &Connection, account_id: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET paused = 1 WHERE id = ?1",
        params![account_id],
    )?;
    Ok(())
}

/// Resume a paused account.
pub fn resume(conn: &Connection, account_id: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET paused = 0 WHERE id = ?1",
        params![account_id],
    )?;
    Ok(())
}

/// Reset session start and counter.
pub fn reset_session(conn: &Connection, account_id: &str, timestamp: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET session_start = ?1, session_request_count = 0 WHERE id = ?2",
        params![timestamp, account_id],
    )?;
    Ok(())
}

/// Update the session request count directly.
pub fn update_request_count(
    conn: &Connection,
    account_id: &str,
    count: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET session_request_count = ?1 WHERE id = ?2",
        params![count, account_id],
    )?;
    Ok(())
}

/// Rename an account.
pub fn rename(conn: &Connection, account_id: &str, new_name: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET name = ?1 WHERE id = ?2",
        params![new_name, account_id],
    )?;
    Ok(())
}

/// Update account priority.
pub fn update_priority(conn: &Connection, account_id: &str, priority: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET priority = ?1 WHERE id = ?2",
        params![priority, account_id],
    )?;
    Ok(())
}

/// Set auto-fallback enabled/disabled.
pub fn set_auto_fallback_enabled(
    conn: &Connection,
    account_id: &str,
    enabled: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET auto_fallback_enabled = ?1 WHERE id = ?2",
        params![enabled as i64, account_id],
    )?;
    Ok(())
}

/// Set auto-refresh enabled/disabled.
pub fn set_auto_refresh_enabled(
    conn: &Connection,
    account_id: &str,
    enabled: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET auto_refresh_enabled = ?1 WHERE id = ?2",
        params![enabled as i64, account_id],
    )?;
    Ok(())
}

/// Set custom endpoint URL (or clear with None).
pub fn set_custom_endpoint(
    conn: &Connection,
    account_id: &str,
    endpoint: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET custom_endpoint = ?1 WHERE id = ?2",
        params![endpoint, account_id],
    )?;
    Ok(())
}

/// Set model mappings JSON string (or clear with None).
pub fn set_model_mappings(
    conn: &Connection,
    account_id: &str,
    mappings: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET model_mappings = ?1 WHERE id = ?2",
        params![mappings, account_id],
    )?;
    Ok(())
}

/// Set 5-hour reserve percent (0-100).
pub fn set_reserve_5h(
    conn: &Connection,
    account_id: &str,
    percent: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET reserve_5h = ?1 WHERE id = ?2",
        params![percent, account_id],
    )?;
    Ok(())
}

/// Set weekly reserve percent (0-100).
pub fn set_reserve_weekly(
    conn: &Connection,
    account_id: &str,
    percent: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET reserve_weekly = ?1 WHERE id = ?2",
        params![percent, account_id],
    )?;
    Ok(())
}

/// Set reserve hard mode (strict exclusion when at reserve threshold).
pub fn set_reserve_hard(
    conn: &Connection,
    account_id: &str,
    hard: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE accounts SET reserve_hard = ?1 WHERE id = ?2",
        params![hard as i64, account_id],
    )?;
    Ok(())
}

/// Clear expired rate limits from all accounts.
///
/// Returns the number of accounts that had their rate_limited_until cleared.
pub fn clear_expired_rate_limits(conn: &Connection, now: i64) -> Result<usize, DbError> {
    let changes = conn.execute(
        "UPDATE accounts SET rate_limited_until = NULL WHERE rate_limited_until <= ?1",
        params![now],
    )?;
    Ok(changes)
}

/// Check if there are any accounts for a specific provider.
pub fn has_accounts_for_provider(conn: &Connection, provider: &str) -> Result<bool, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE provider = ?1",
        params![provider],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Delete an account by ID.
pub fn delete(conn: &Connection, account_id: &str) -> Result<bool, DbError> {
    let changes = conn.execute("DELETE FROM accounts WHERE id = ?1", params![account_id])?;
    Ok(changes > 0)
}

/// Create a new account.
pub fn create(conn: &Connection, account: &Account) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO accounts (
            id, name, provider, api_key, refresh_token, access_token,
            expires_at, request_count, total_requests, last_used, created_at,
            rate_limited_until, session_start, session_request_count, paused,
            rate_limit_reset, rate_limit_status, rate_limit_remaining,
            priority, auto_fallback_enabled, auto_refresh_enabled,
            custom_endpoint, model_mappings, reserve_5h, reserve_weekly, reserve_hard
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
        )",
        params![
            account.id,
            account.name,
            account.provider,
            account.api_key,
            account.refresh_token,
            account.access_token,
            account.expires_at,
            account.request_count,
            account.total_requests,
            account.last_used,
            account.created_at,
            account.rate_limited_until,
            account.session_start,
            account.session_request_count,
            account.paused as i64,
            account.rate_limit_reset,
            account.rate_limit_status,
            account.rate_limit_remaining,
            account.priority,
            account.auto_fallback_enabled as i64,
            account.auto_refresh_enabled as i64,
            account.custom_endpoint,
            account.model_mappings,
            account.reserve_5h,
            account.reserve_weekly,
            account.reserve_hard as i64,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_tables(&conn).unwrap();
        schema::create_indexes(&conn).unwrap();
        conn
    }

    fn test_account(id: &str, name: &str) -> Account {
        Account {
            id: id.to_string(),
            name: name.to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: "rt_test".to_string(),
            access_token: Some("at_test".to_string()),
            expires_at: Some(9999999999999),
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: 1700000000000,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
        }
    }

    #[test]
    fn create_and_find_all() {
        let conn = setup_db();
        let acct = test_account("acc1", "Test Account");
        create(&conn, &acct).unwrap();

        let all = find_all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Test Account");
    }

    #[test]
    fn find_by_id_found_and_missing() {
        let conn = setup_db();
        let acct = test_account("acc1", "Test");
        create(&conn, &acct).unwrap();

        assert!(find_by_id(&conn, "acc1").unwrap().is_some());
        assert!(find_by_id(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn update_tokens_without_refresh() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        update_tokens(&conn, "acc1", "new_at", 999, None).unwrap();
        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.access_token.as_deref(), Some("new_at"));
        assert_eq!(acct.expires_at, Some(999));
        assert_eq!(acct.refresh_token, "rt_test"); // unchanged
    }

    #[test]
    fn update_tokens_with_refresh() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        update_tokens(&conn, "acc1", "new_at", 888, Some("new_rt")).unwrap();
        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.refresh_token, "new_rt");
    }

    #[test]
    fn increment_usage_new_session() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        let now = 1700000100000_i64;
        let session_dur = 18000000_i64; // 5h
        increment_usage(&conn, "acc1", now, session_dur).unwrap();

        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.request_count, 1);
        assert_eq!(acct.total_requests, 1);
        assert_eq!(acct.session_request_count, 1);
        assert_eq!(acct.session_start, Some(now));
        assert_eq!(acct.last_used, Some(now));
    }

    #[test]
    fn pause_and_resume() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        pause(&conn, "acc1").unwrap();
        assert!(find_by_id(&conn, "acc1").unwrap().unwrap().paused);

        resume(&conn, "acc1").unwrap();
        assert!(!find_by_id(&conn, "acc1").unwrap().unwrap().paused);
    }

    #[test]
    fn rename_account() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Old Name")).unwrap();

        rename(&conn, "acc1", "New Name").unwrap();
        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.name, "New Name");
    }

    #[test]
    fn priority_and_ordering() {
        let conn = setup_db();
        let mut a1 = test_account("a1", "Low");
        a1.priority = 1;
        let mut a2 = test_account("a2", "High");
        a2.priority = 10;
        create(&conn, &a1).unwrap();
        create(&conn, &a2).unwrap();

        let all = find_all(&conn).unwrap();
        assert_eq!(all[0].name, "High"); // higher priority first
        assert_eq!(all[1].name, "Low");
    }

    #[test]
    fn clear_expired_rate_limits_works() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();
        set_rate_limited(&conn, "acc1", 1000).unwrap();

        let cleared = clear_expired_rate_limits(&conn, 2000).unwrap();
        assert_eq!(cleared, 1);

        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(acct.rate_limited_until.is_none());
    }

    #[test]
    fn has_accounts_for_provider_works() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        assert!(has_accounts_for_provider(&conn, "anthropic").unwrap());
        assert!(!has_accounts_for_provider(&conn, "openai").unwrap());
    }

    #[test]
    fn delete_account() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        assert!(delete(&conn, "acc1").unwrap());
        assert!(!delete(&conn, "acc1").unwrap()); // already gone
        assert!(find_by_id(&conn, "acc1").unwrap().is_none());
    }

    #[test]
    fn set_auto_fallback() {
        let conn = setup_db();
        create(&conn, &test_account("acc1", "Test")).unwrap();

        set_auto_fallback_enabled(&conn, "acc1", false).unwrap();
        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(!acct.auto_fallback_enabled);

        set_auto_fallback_enabled(&conn, "acc1", true).unwrap();
        let acct = find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(acct.auto_fallback_enabled);
    }
}
