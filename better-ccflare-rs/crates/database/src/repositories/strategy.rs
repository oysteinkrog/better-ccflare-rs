//! Strategy repository — persist/load strategy configuration.
//!
//! Matches the TypeScript `StrategyRepository`.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::DbError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A persisted strategy with its JSON config.
#[derive(Debug, Clone)]
pub struct StrategyData {
    pub name: String,
    /// JSON-encoded configuration object.
    pub config: String,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// Read operations
// ---------------------------------------------------------------------------

/// Get a strategy by name, returning its parsed config.
pub fn get_strategy(conn: &Connection, name: &str) -> Result<Option<StrategyData>, DbError> {
    let result = conn
        .query_row(
            "SELECT name, config, updated_at FROM strategies WHERE name = ?1",
            params![name],
            |row| {
                Ok(StrategyData {
                    name: row.get(0)?,
                    config: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

/// List all strategies ordered by name.
pub fn list(conn: &Connection) -> Result<Vec<StrategyData>, DbError> {
    let mut stmt = conn.prepare("SELECT name, config, updated_at FROM strategies ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok(StrategyData {
            name: row.get(0)?,
            config: row.get(1)?,
            updated_at: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Write operations
// ---------------------------------------------------------------------------

/// Set (insert or replace) a strategy's config.
pub fn set(conn: &Connection, name: &str, config_json: &str, now: i64) -> Result<(), DbError> {
    conn.execute(
        "INSERT OR REPLACE INTO strategies (name, config, updated_at) VALUES (?1, ?2, ?3)",
        params![name, config_json, now],
    )?;
    Ok(())
}

/// Delete a strategy by name.
pub fn delete(conn: &Connection, name: &str) -> Result<bool, DbError> {
    let changes = conn.execute("DELETE FROM strategies WHERE name = ?1", params![name])?;
    Ok(changes > 0)
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
    fn set_and_get_strategy() {
        let conn = setup_db();
        let now = 1700000000000_i64;
        let config = r#"{"sessionDuration":18000000}"#;

        set(&conn, "session", config, now).unwrap();

        let strategy = get_strategy(&conn, "session").unwrap().unwrap();
        assert_eq!(strategy.name, "session");
        assert_eq!(strategy.config, config);
        assert_eq!(strategy.updated_at, now);
    }

    #[test]
    fn get_strategy_missing() {
        let conn = setup_db();
        assert!(get_strategy(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn set_replaces_existing() {
        let conn = setup_db();
        set(&conn, "session", r#"{"v":1}"#, 1000).unwrap();
        set(&conn, "session", r#"{"v":2}"#, 2000).unwrap();

        let strategy = get_strategy(&conn, "session").unwrap().unwrap();
        assert_eq!(strategy.config, r#"{"v":2}"#);
        assert_eq!(strategy.updated_at, 2000);
    }

    #[test]
    fn list_strategies() {
        let conn = setup_db();
        set(&conn, "beta", r#"{}"#, 1000).unwrap();
        set(&conn, "alpha", r#"{}"#, 2000).unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "alpha"); // ordered by name
        assert_eq!(all[1].name, "beta");
    }

    #[test]
    fn delete_strategy() {
        let conn = setup_db();
        set(&conn, "session", r#"{}"#, 1000).unwrap();

        assert!(delete(&conn, "session").unwrap());
        assert!(!delete(&conn, "session").unwrap()); // already gone
        assert!(get_strategy(&conn, "session").unwrap().is_none());
    }
}
