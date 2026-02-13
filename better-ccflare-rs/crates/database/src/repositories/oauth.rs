//! OAuth session repository — CRUD operations for the `oauth_sessions` table.
//!
//! All SQL queries match the TypeScript `OAuthRepository` exactly.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::DbError;

/// An OAuth PKCE session stored during the authorization flow.
#[derive(Debug, Clone)]
pub struct OAuthSession {
    pub account_name: String,
    pub verifier: String,
    pub mode: String,
    pub custom_endpoint: Option<String>,
}

/// Create a new OAuth session with a TTL.
///
/// The session expires after `ttl_minutes` (default 10).
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    conn: &Connection,
    session_id: &str,
    account_name: &str,
    verifier: &str,
    mode: &str,
    custom_endpoint: Option<&str>,
    now: i64,
    ttl_minutes: i64,
) -> Result<(), DbError> {
    let expires_at = now + ttl_minutes * 60 * 1000;

    conn.execute(
        "INSERT INTO oauth_sessions (id, account_name, verifier, mode, custom_endpoint, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![session_id, account_name, verifier, mode, custom_endpoint, now, expires_at],
    )?;
    Ok(())
}

/// Retrieve a session by ID if it hasn't expired.
pub fn get_session(
    conn: &Connection,
    session_id: &str,
    now: i64,
) -> Result<Option<OAuthSession>, DbError> {
    let result = conn
        .query_row(
            "SELECT account_name, verifier, mode, custom_endpoint, expires_at
             FROM oauth_sessions
             WHERE id = ?1 AND expires_at > ?2",
            params![session_id, now],
            |row| {
                Ok(OAuthSession {
                    account_name: row.get("account_name")?,
                    verifier: row.get("verifier")?,
                    mode: row.get("mode")?,
                    custom_endpoint: row.get("custom_endpoint")?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

/// Delete a specific session.
pub fn delete_session(conn: &Connection, session_id: &str) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM oauth_sessions WHERE id = ?1",
        params![session_id],
    )?;
    Ok(())
}

/// Delete all expired sessions.
///
/// Returns the number of sessions cleaned up.
pub fn cleanup_expired_sessions(conn: &Connection, now: i64) -> Result<usize, DbError> {
    let changes = conn.execute(
        "DELETE FROM oauth_sessions WHERE expires_at <= ?1",
        params![now],
    )?;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn create_and_get_session() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        create_session(
            &conn,
            "state123",
            "my-account",
            "verifier_xyz",
            "claude-oauth",
            None,
            now,
            10,
        )
        .unwrap();

        // Session should be retrievable before expiry
        let session = get_session(&conn, "state123", now + 1000).unwrap();
        assert!(session.is_some());
        let s = session.unwrap();
        assert_eq!(s.account_name, "my-account");
        assert_eq!(s.verifier, "verifier_xyz");
        assert_eq!(s.mode, "claude-oauth");
        assert!(s.custom_endpoint.is_none());
    }

    #[test]
    fn get_session_expired() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        create_session(
            &conn, "state123", "acct", "verifier", "console", None, now, 10,
        )
        .unwrap();

        // After 10 minutes + 1ms, session should be expired
        let after_expiry = now + 10 * 60 * 1000 + 1;
        let session = get_session(&conn, "state123", after_expiry).unwrap();
        assert!(session.is_none());
    }

    #[test]
    fn delete_session_works() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        create_session(
            &conn, "state123", "acct", "verifier", "console", None, now, 10,
        )
        .unwrap();

        delete_session(&conn, "state123").unwrap();
        let session = get_session(&conn, "state123", now).unwrap();
        assert!(session.is_none());
    }

    #[test]
    fn cleanup_expired_sessions_works() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        // Create 2 sessions with different expiry
        create_session(&conn, "s1", "acct1", "v1", "console", None, now, 5).unwrap();
        create_session(&conn, "s2", "acct2", "v2", "claude-oauth", None, now, 20).unwrap();

        // At now + 6 minutes, s1 should be expired, s2 still valid
        let cleanup_time = now + 6 * 60 * 1000;
        let cleaned = cleanup_expired_sessions(&conn, cleanup_time).unwrap();
        assert_eq!(cleaned, 1);

        // s2 should still be accessible
        assert!(get_session(&conn, "s2", cleanup_time).unwrap().is_some());
    }

    #[test]
    fn session_with_custom_endpoint() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        create_session(
            &conn,
            "state123",
            "acct",
            "verifier",
            "claude-oauth",
            Some("https://custom.example.com"),
            now,
            10,
        )
        .unwrap();

        let session = get_session(&conn, "state123", now).unwrap().unwrap();
        assert_eq!(
            session.custom_endpoint.as_deref(),
            Some("https://custom.example.com")
        );
    }
}
