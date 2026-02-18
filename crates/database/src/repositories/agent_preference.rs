//! Agent preference repository — per-agent model preferences.
//!
//! Matches the TypeScript `AgentPreferenceRepository`.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::DbError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single agent's model preference.
#[derive(Debug, Clone)]
pub struct AgentPreference {
    pub agent_id: String,
    pub preferred_model: String,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// Read operations
// ---------------------------------------------------------------------------

/// Get model preference for a specific agent.
pub fn get_preference(conn: &Connection, agent_id: &str) -> Result<Option<String>, DbError> {
    let result = conn
        .query_row(
            "SELECT preferred_model FROM agent_preferences WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(result)
}

/// Get all agent preferences.
pub fn get_all_preferences(conn: &Connection) -> Result<Vec<AgentPreference>, DbError> {
    let mut stmt =
        conn.prepare("SELECT agent_id, preferred_model, updated_at FROM agent_preferences")?;
    let rows = stmt.query_map([], |row| {
        Ok(AgentPreference {
            agent_id: row.get(0)?,
            preferred_model: row.get(1)?,
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

/// Set (insert or replace) model preference for an agent.
pub fn set_preference(
    conn: &Connection,
    agent_id: &str,
    model: &str,
    now: i64,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO agent_preferences (agent_id, preferred_model, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(agent_id) DO UPDATE SET
             preferred_model = excluded.preferred_model,
             updated_at = excluded.updated_at",
        params![agent_id, model, now],
    )?;
    Ok(())
}

/// Delete preference for a specific agent.
pub fn delete_preference(conn: &Connection, agent_id: &str) -> Result<bool, DbError> {
    let changes = conn.execute(
        "DELETE FROM agent_preferences WHERE agent_id = ?1",
        params![agent_id],
    )?;
    Ok(changes > 0)
}

/// Set preferences for multiple agents in bulk.
///
/// Uses a transaction for atomicity.
pub fn set_bulk_preferences(
    conn: &Connection,
    agent_ids: &[&str],
    model: &str,
    now: i64,
) -> Result<(), DbError> {
    if agent_ids.is_empty() {
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO agent_preferences (agent_id, preferred_model, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id) DO UPDATE SET
                 preferred_model = excluded.preferred_model,
                 updated_at = excluded.updated_at",
        )?;
        for &agent_id in agent_ids {
            stmt.execute(params![agent_id, model, now])?;
        }
    }
    tx.commit()?;
    Ok(())
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
    fn set_and_get_preference() {
        let conn = setup_db();
        let now = 1700000000000_i64;

        set_preference(&conn, "agent-1", "claude-3-opus", now).unwrap();

        let model = get_preference(&conn, "agent-1").unwrap();
        assert_eq!(model.as_deref(), Some("claude-3-opus"));
    }

    #[test]
    fn get_preference_missing() {
        let conn = setup_db();
        assert!(get_preference(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn set_preference_replaces() {
        let conn = setup_db();
        set_preference(&conn, "agent-1", "claude-3-opus", 1000).unwrap();
        set_preference(&conn, "agent-1", "claude-3-5-sonnet", 2000).unwrap();

        let model = get_preference(&conn, "agent-1").unwrap().unwrap();
        assert_eq!(model, "claude-3-5-sonnet");
    }

    #[test]
    fn get_all_preferences_works() {
        let conn = setup_db();
        set_preference(&conn, "a1", "model-a", 1000).unwrap();
        set_preference(&conn, "a2", "model-b", 2000).unwrap();

        let all = get_all_preferences(&conn).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn delete_preference_works() {
        let conn = setup_db();
        set_preference(&conn, "agent-1", "claude-3-opus", 1000).unwrap();

        assert!(delete_preference(&conn, "agent-1").unwrap());
        assert!(!delete_preference(&conn, "agent-1").unwrap()); // already gone
        assert!(get_preference(&conn, "agent-1").unwrap().is_none());
    }

    #[test]
    fn set_bulk_preferences_works() {
        let conn = setup_db();
        let now = 1700000000000_i64;
        let agents = vec!["a1", "a2", "a3"];

        set_bulk_preferences(&conn, &agents, "claude-3-opus", now).unwrap();

        let all = get_all_preferences(&conn).unwrap();
        assert_eq!(all.len(), 3);
        for pref in &all {
            assert_eq!(pref.preferred_model, "claude-3-opus");
        }
    }

    #[test]
    fn set_bulk_empty_is_noop() {
        let conn = setup_db();
        set_bulk_preferences(&conn, &[], "model", 1000).unwrap();
        assert_eq!(get_all_preferences(&conn).unwrap().len(), 0);
    }
}
