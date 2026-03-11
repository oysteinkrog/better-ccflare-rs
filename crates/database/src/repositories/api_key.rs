//! API key repository — CRUD operations for the `api_keys` table.
//!
//! All SQL queries match the TypeScript `ApiKeyRepository` exactly.

use rusqlite::{params, Connection, OptionalExtension};

use bccf_core::types::{ApiKey, KeyScope};

use crate::error::DbError;

// ---------------------------------------------------------------------------
// Row mapper
// ---------------------------------------------------------------------------

fn row_to_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKey> {
    Ok(ApiKey {
        id: row.get("id")?,
        name: row.get("name")?,
        hashed_key: row.get("hashed_key")?,
        prefix_last_8: row.get("prefix_last_8")?,
        created_at: row.get("created_at")?,
        last_used: row.get("last_used")?,
        usage_count: row.get::<_, Option<i64>>("usage_count")?.unwrap_or(0),
        is_active: row.get::<_, i64>("is_active")? != 0,
        scope: {
            let s: String = row.get::<_, Option<String>>("scope")?.unwrap_or_default();
            KeyScope::from_db(&s)
        },
    })
}

const API_KEY_SELECT: &str = "
    SELECT id, name, hashed_key, prefix_last_8, created_at,
           last_used, usage_count, is_active, scope
    FROM api_keys
";

// ---------------------------------------------------------------------------
// Read operations
// ---------------------------------------------------------------------------

/// Find all API keys ordered by creation date (newest first).
pub fn find_all(conn: &Connection) -> Result<Vec<ApiKey>, DbError> {
    let sql = format!("{API_KEY_SELECT} ORDER BY created_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_api_key)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Find only active API keys.
pub fn find_active(conn: &Connection) -> Result<Vec<ApiKey>, DbError> {
    let sql = format!("{API_KEY_SELECT} WHERE is_active = 1 ORDER BY created_at DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_api_key)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Find an API key by ID.
pub fn find_by_id(conn: &Connection, id: &str) -> Result<Option<ApiKey>, DbError> {
    let sql = format!("{API_KEY_SELECT} WHERE id = ?1");
    let result = conn
        .query_row(&sql, params![id], row_to_api_key)
        .optional()?;
    Ok(result)
}

/// Find an active API key by its hashed key (for authentication).
pub fn find_by_hashed_key(conn: &Connection, hashed_key: &str) -> Result<Option<ApiKey>, DbError> {
    let sql = format!("{API_KEY_SELECT} WHERE hashed_key = ?1 AND is_active = 1");
    let result = conn
        .query_row(&sql, params![hashed_key], row_to_api_key)
        .optional()?;
    Ok(result)
}

/// Find an API key by name.
pub fn find_by_name(conn: &Connection, name: &str) -> Result<Option<ApiKey>, DbError> {
    let sql = format!("{API_KEY_SELECT} WHERE name = ?1");
    let result = conn
        .query_row(&sql, params![name], row_to_api_key)
        .optional()?;
    Ok(result)
}

/// Check if an API key name already exists.
pub fn name_exists(conn: &Connection, name: &str) -> Result<bool, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Count active API keys.
pub fn count_active(conn: &Connection) -> Result<i64, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE is_active = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count all API keys (active and inactive).
pub fn count_all(conn: &Connection) -> Result<i64, DbError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM api_keys", [], |row| row.get(0))?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Write operations
// ---------------------------------------------------------------------------

/// Create a new API key.
pub fn create(conn: &Connection, key: &ApiKey) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO api_keys (id, name, hashed_key, prefix_last_8, created_at, last_used, is_active, scope)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            key.id,
            key.name,
            key.hashed_key,
            key.prefix_last_8,
            key.created_at,
            key.last_used,
            key.is_active as i64,
            key.scope.as_db(),
        ],
    )?;
    Ok(())
}

/// Update last_used timestamp and increment usage count.
pub fn update_usage(conn: &Connection, id: &str, timestamp: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE api_keys SET last_used = ?1, usage_count = usage_count + 1 WHERE id = ?2",
        params![timestamp, id],
    )?;
    Ok(())
}

/// Disable (soft-delete) an API key.
pub fn disable(conn: &Connection, id: &str) -> Result<bool, DbError> {
    let changes = conn.execute(
        "UPDATE api_keys SET is_active = 0 WHERE id = ?1",
        params![id],
    )?;
    Ok(changes > 0)
}

/// Re-enable a disabled API key.
pub fn enable(conn: &Connection, id: &str) -> Result<bool, DbError> {
    let changes = conn.execute(
        "UPDATE api_keys SET is_active = 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(changes > 0)
}

/// Permanently delete an API key.
pub fn delete(conn: &Connection, id: &str) -> Result<bool, DbError> {
    let changes = conn.execute("DELETE FROM api_keys WHERE id = ?1", params![id])?;
    Ok(changes > 0)
}

/// Delete all API keys.
pub fn clear_all(conn: &Connection) -> Result<(), DbError> {
    conn.execute("DELETE FROM api_keys", [])?;
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

    fn test_key(id: &str, name: &str) -> ApiKey {
        ApiKey {
            id: id.to_string(),
            name: name.to_string(),
            hashed_key: format!("hash_{id}"),
            prefix_last_8: "bccf_...abcd1234".to_string(),
            created_at: 1700000000000,
            last_used: None,
            usage_count: 0,
            is_active: true,
            scope: KeyScope::Admin,
        }
    }

    #[test]
    fn create_and_find_all() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Key One")).unwrap();
        create(&conn, &test_key("k2", "Key Two")).unwrap();

        let all = find_all(&conn).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn find_active_excludes_disabled() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Active")).unwrap();
        let mut disabled = test_key("k2", "Disabled");
        disabled.is_active = false;
        create(&conn, &disabled).unwrap();

        let active = find_active(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "Active");
    }

    #[test]
    fn find_by_id_and_name() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "My Key")).unwrap();

        assert!(find_by_id(&conn, "k1").unwrap().is_some());
        assert!(find_by_id(&conn, "missing").unwrap().is_none());
        assert!(find_by_name(&conn, "My Key").unwrap().is_some());
    }

    #[test]
    fn find_by_hashed_key_active_only() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Key")).unwrap();
        assert!(find_by_hashed_key(&conn, "hash_k1").unwrap().is_some());

        disable(&conn, "k1").unwrap();
        assert!(find_by_hashed_key(&conn, "hash_k1").unwrap().is_none());
    }

    #[test]
    fn name_exists_works() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Taken")).unwrap();
        assert!(name_exists(&conn, "Taken").unwrap());
        assert!(!name_exists(&conn, "Available").unwrap());
    }

    #[test]
    fn update_usage_increments() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Key")).unwrap();

        update_usage(&conn, "k1", 1700000001000).unwrap();
        update_usage(&conn, "k1", 1700000002000).unwrap();

        let key = find_by_id(&conn, "k1").unwrap().unwrap();
        assert_eq!(key.usage_count, 2);
        assert_eq!(key.last_used, Some(1700000002000));
    }

    #[test]
    fn disable_and_enable() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Key")).unwrap();

        assert!(disable(&conn, "k1").unwrap());
        assert!(!find_by_id(&conn, "k1").unwrap().unwrap().is_active);

        assert!(enable(&conn, "k1").unwrap());
        assert!(find_by_id(&conn, "k1").unwrap().unwrap().is_active);
    }

    #[test]
    fn delete_key() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Key")).unwrap();

        assert!(delete(&conn, "k1").unwrap());
        assert!(!delete(&conn, "k1").unwrap()); // already gone
        assert!(find_by_id(&conn, "k1").unwrap().is_none());
    }

    #[test]
    fn count_operations() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "Active1")).unwrap();
        create(&conn, &test_key("k2", "Active2")).unwrap();
        let mut disabled = test_key("k3", "Disabled");
        disabled.is_active = false;
        create(&conn, &disabled).unwrap();

        assert_eq!(count_active(&conn).unwrap(), 2);
        assert_eq!(count_all(&conn).unwrap(), 3);
    }

    #[test]
    fn clear_all_works() {
        let conn = setup_db();
        create(&conn, &test_key("k1", "A")).unwrap();
        create(&conn, &test_key("k2", "B")).unwrap();

        clear_all(&conn).unwrap();
        assert_eq!(count_all(&conn).unwrap(), 0);
    }
}
