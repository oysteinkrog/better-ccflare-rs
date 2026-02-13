//! Request repository — CRUD and analytics for the `requests` and `request_payloads` tables.
//!
//! All SQL queries match the TypeScript `RequestRepository` exactly.

use rusqlite::{params, Connection, OptionalExtension};

use bccf_core::types::ProxyRequest;

use crate::error::DbError;

/// (request_body, response_body) pair from `request_payloads`.
pub type PayloadPair = (Option<String>, Option<String>);

// ---------------------------------------------------------------------------
// Row mapper
// ---------------------------------------------------------------------------

fn row_to_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxyRequest> {
    Ok(ProxyRequest {
        id: row.get("id")?,
        timestamp: row.get("timestamp")?,
        method: row.get("method")?,
        path: row.get("path")?,
        account_used: row.get("account_used")?,
        status_code: row.get("status_code")?,
        success: row.get::<_, i64>("success")? != 0,
        error_message: row.get("error_message")?,
        response_time_ms: row.get("response_time_ms")?,
        failover_attempts: row.get::<_, Option<i64>>("failover_attempts")?.unwrap_or(0),
        model: row.get("model")?,
        prompt_tokens: row.get("prompt_tokens")?,
        completion_tokens: row.get("completion_tokens")?,
        total_tokens: row.get("total_tokens")?,
        cost_usd: row.get("cost_usd")?,
        input_tokens: row.get("input_tokens")?,
        cache_read_input_tokens: row.get("cache_read_input_tokens")?,
        cache_creation_input_tokens: row.get("cache_creation_input_tokens")?,
        output_tokens: row.get("output_tokens")?,
        agent_used: row.get("agent_used")?,
        tokens_per_second: row.get("tokens_per_second")?,
        project: row.get("project")?,
        api_key_id: row.get("api_key_id")?,
        api_key_name: row.get("api_key_name")?,
    })
}

// ---------------------------------------------------------------------------
// Write operations
// ---------------------------------------------------------------------------

/// Save initial request metadata (before the response is complete).
#[allow(clippy::too_many_arguments)]
pub fn save_meta(
    conn: &Connection,
    id: &str,
    method: &str,
    path: &str,
    account_used: Option<&str>,
    status_code: Option<i64>,
    timestamp: i64,
    api_key_id: Option<&str>,
    api_key_name: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO requests (
            id, timestamp, method, path, account_used,
            status_code, success, error_message, response_time_ms, failover_attempts,
            api_key_id, api_key_name
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, NULL, 0, 0, ?7, ?8)",
        params![
            id,
            timestamp,
            method,
            path,
            account_used,
            status_code,
            api_key_id,
            api_key_name
        ],
    )?;
    Ok(())
}

/// Insert or replace a full request record.
pub fn save(conn: &Connection, req: &ProxyRequest) -> Result<(), DbError> {
    conn.execute(
        "INSERT OR REPLACE INTO requests (
            id, timestamp, method, path, account_used,
            status_code, success, error_message, response_time_ms, failover_attempts,
            model, prompt_tokens, completion_tokens, total_tokens, cost_usd,
            input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens,
            agent_used, tokens_per_second, project, api_key_id, api_key_name
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        params![
            req.id,
            req.timestamp,
            req.method,
            req.path,
            req.account_used,
            req.status_code,
            req.success as i64,
            req.error_message,
            req.response_time_ms,
            req.failover_attempts,
            req.model,
            req.prompt_tokens,
            req.completion_tokens,
            req.total_tokens,
            req.cost_usd,
            req.input_tokens,
            req.cache_read_input_tokens,
            req.cache_creation_input_tokens,
            req.output_tokens,
            req.agent_used,
            req.tokens_per_second,
            req.project,
            req.api_key_id,
            req.api_key_name,
        ],
    )?;
    Ok(())
}

/// Usage data for updating a request after response completes.
#[derive(Debug, Clone, Default)]
pub struct UsageUpdate {
    pub model: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub tokens_per_second: Option<f64>,
}

/// Update usage/token fields using COALESCE to avoid overwriting existing values.
pub fn update_usage(
    conn: &Connection,
    request_id: &str,
    usage: &UsageUpdate,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE requests
         SET
             model = COALESCE(?1, model),
             prompt_tokens = COALESCE(?2, prompt_tokens),
             completion_tokens = COALESCE(?3, completion_tokens),
             total_tokens = COALESCE(?4, total_tokens),
             cost_usd = COALESCE(?5, cost_usd),
             input_tokens = COALESCE(?6, input_tokens),
             cache_read_input_tokens = COALESCE(?7, cache_read_input_tokens),
             cache_creation_input_tokens = COALESCE(?8, cache_creation_input_tokens),
             output_tokens = COALESCE(?9, output_tokens),
             tokens_per_second = COALESCE(?10, tokens_per_second)
         WHERE id = ?11",
        params![
            usage.model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens,
            usage.cost_usd,
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            usage.output_tokens,
            usage.tokens_per_second,
            request_id,
        ],
    )?;
    Ok(())
}

/// Update the success flag, error message, and response time after a request completes.
pub fn update_result(
    conn: &Connection,
    request_id: &str,
    success: bool,
    error_message: Option<&str>,
    response_time_ms: i64,
    failover_attempts: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE requests
         SET success = ?1, error_message = ?2, response_time_ms = ?3, failover_attempts = ?4
         WHERE id = ?5",
        params![
            success as i64,
            error_message,
            response_time_ms,
            failover_attempts,
            request_id,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Payload management
// ---------------------------------------------------------------------------

/// Store a request/response payload (JSON blobs).
pub fn save_payload(
    conn: &Connection,
    request_id: &str,
    request_body: Option<&str>,
    response_body: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT OR REPLACE INTO request_payloads (request_id, request_body, response_body)
         VALUES (?1, ?2, ?3)",
        params![request_id, request_body, response_body],
    )?;
    Ok(())
}

/// Retrieve payload for a request.
pub fn get_payload(conn: &Connection, request_id: &str) -> Result<Option<PayloadPair>, DbError> {
    let result = conn
        .query_row(
            "SELECT request_body, response_body FROM request_payloads WHERE request_id = ?1",
            params![request_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(result)
}

/// Delete payloads older than cutoff (by joining with requests timestamp).
pub fn delete_payloads_older_than(conn: &Connection, cutoff_ts: i64) -> Result<usize, DbError> {
    let changes = conn.execute(
        "DELETE FROM request_payloads
         WHERE request_id IN (SELECT id FROM requests WHERE timestamp < ?1)",
        params![cutoff_ts],
    )?;
    Ok(changes)
}

/// Delete orphaned payloads (payload exists but request was already deleted).
pub fn delete_orphaned_payloads(conn: &Connection) -> Result<usize, DbError> {
    let changes = conn.execute(
        "DELETE FROM request_payloads
         WHERE request_id NOT IN (SELECT id FROM requests)",
        [],
    )?;
    Ok(changes)
}

// ---------------------------------------------------------------------------
// Read / analytics
// ---------------------------------------------------------------------------

/// Fetch a request by ID.
pub fn find_by_id(conn: &Connection, request_id: &str) -> Result<Option<ProxyRequest>, DbError> {
    let sql = "SELECT * FROM requests WHERE id = ?1";
    let result = conn
        .query_row(sql, params![request_id], row_to_request)
        .optional()?;
    Ok(result)
}

/// Fetch the most recent requests.
pub fn get_recent(conn: &Connection, limit: i64) -> Result<Vec<ProxyRequest>, DbError> {
    let mut stmt = conn.prepare("SELECT * FROM requests ORDER BY timestamp DESC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit], row_to_request)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Basic request statistics with optional time filter.
#[derive(Debug, Clone)]
pub struct RequestStats {
    pub total_requests: i64,
    pub successful_requests: i64,
    pub failed_requests: i64,
    pub avg_response_time: Option<f64>,
}

pub fn get_request_stats(conn: &Connection, since: Option<i64>) -> Result<RequestStats, DbError> {
    let (sql, p): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ts) = since {
        (
            "SELECT
                COUNT(*) as total_requests,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successful_requests,
                SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed_requests,
                AVG(response_time_ms) as avg_response_time
             FROM requests
             WHERE timestamp > ?1"
                .to_string(),
            vec![Box::new(ts)],
        )
    } else {
        (
            "SELECT
                COUNT(*) as total_requests,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successful_requests,
                SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) as failed_requests,
                AVG(response_time_ms) as avg_response_time
             FROM requests"
                .to_string(),
            vec![],
        )
    };

    let result = conn.query_row(&sql, rusqlite::params_from_iter(p.iter()), |row| {
        Ok(RequestStats {
            total_requests: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
            successful_requests: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            failed_requests: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            avg_response_time: row.get(3)?,
        })
    })?;
    Ok(result)
}

/// Top models by usage count.
pub fn get_top_models(conn: &Connection, limit: i64) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT model, COUNT(*) as count
         FROM requests
         WHERE model IS NOT NULL
         GROUP BY model
         ORDER BY count DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Recent error messages.
pub fn get_recent_errors(conn: &Connection, limit: i64) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT error_message
         FROM requests
         WHERE success = 0 AND error_message IS NOT NULL
         ORDER BY timestamp DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Delete requests older than the given timestamp.
pub fn delete_older_than(conn: &Connection, cutoff_ts: i64) -> Result<usize, DbError> {
    let changes = conn.execute(
        "DELETE FROM requests WHERE timestamp < ?1",
        params![cutoff_ts],
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
        schema::create_indexes(&conn).unwrap();
        conn
    }

    fn test_request(id: &str) -> ProxyRequest {
        ProxyRequest {
            id: id.to_string(),
            timestamp: 1700000000000,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            account_used: Some("acc1".to_string()),
            status_code: Some(200),
            success: true,
            error_message: None,
            response_time_ms: Some(150),
            failover_attempts: 0,
            model: Some("claude-3-opus".to_string()),
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            cost_usd: Some(0.01),
            input_tokens: Some(100),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens: Some(50),
            agent_used: None,
            tokens_per_second: Some(33.3),
            project: Some("my-project".to_string()),
            api_key_id: None,
            api_key_name: None,
        }
    }

    #[test]
    fn save_and_find_by_id() {
        let conn = setup_db();
        let req = test_request("req1");
        save(&conn, &req).unwrap();

        let found = find_by_id(&conn, "req1").unwrap().unwrap();
        assert_eq!(found.method, "POST");
        assert_eq!(found.model.as_deref(), Some("claude-3-opus"));
        assert!(found.success);
    }

    #[test]
    fn save_meta_and_update_result() {
        let conn = setup_db();
        save_meta(
            &conn,
            "req2",
            "POST",
            "/v1/messages",
            Some("acc1"),
            None,
            1700000000000,
            None,
            None,
        )
        .unwrap();

        let req = find_by_id(&conn, "req2").unwrap().unwrap();
        assert!(!req.success);
        assert_eq!(req.response_time_ms, Some(0));

        update_result(&conn, "req2", true, None, 200, 1).unwrap();
        let req = find_by_id(&conn, "req2").unwrap().unwrap();
        assert!(req.success);
        assert_eq!(req.response_time_ms, Some(200));
        assert_eq!(req.failover_attempts, 1);
    }

    #[test]
    fn update_usage_works() {
        let conn = setup_db();
        save(&conn, &test_request("req3")).unwrap();

        let usage = UsageUpdate {
            model: Some("claude-3-5-sonnet".to_string()),
            total_tokens: Some(999),
            ..Default::default()
        };
        update_usage(&conn, "req3", &usage).unwrap();

        let req = find_by_id(&conn, "req3").unwrap().unwrap();
        assert_eq!(req.model.as_deref(), Some("claude-3-5-sonnet"));
        assert_eq!(req.total_tokens, Some(999));
        // Original values should be preserved via COALESCE
        assert_eq!(req.prompt_tokens, Some(100));
    }

    #[test]
    fn get_recent_returns_ordered() {
        let conn = setup_db();
        let mut r1 = test_request("r1");
        r1.timestamp = 1000;
        let mut r2 = test_request("r2");
        r2.timestamp = 2000;
        save(&conn, &r1).unwrap();
        save(&conn, &r2).unwrap();

        let recent = get_recent(&conn, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "r2"); // newest first
    }

    #[test]
    fn payload_crud() {
        let conn = setup_db();
        save(&conn, &test_request("req4")).unwrap();

        save_payload(
            &conn,
            "req4",
            Some(r#"{"msg":"hi"}"#),
            Some(r#"{"ok":true}"#),
        )
        .unwrap();
        let (req_body, res_body) = get_payload(&conn, "req4").unwrap().unwrap();
        assert_eq!(req_body.as_deref(), Some(r#"{"msg":"hi"}"#));
        assert_eq!(res_body.as_deref(), Some(r#"{"ok":true}"#));

        assert!(get_payload(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn delete_older_than_works() {
        let conn = setup_db();
        let mut r1 = test_request("old");
        r1.timestamp = 1000;
        let mut r2 = test_request("new");
        r2.timestamp = 5000;
        save(&conn, &r1).unwrap();
        save(&conn, &r2).unwrap();

        let deleted = delete_older_than(&conn, 3000).unwrap();
        assert_eq!(deleted, 1);
        assert!(find_by_id(&conn, "old").unwrap().is_none());
        assert!(find_by_id(&conn, "new").unwrap().is_some());
    }

    #[test]
    fn get_request_stats_works() {
        let conn = setup_db();
        save(&conn, &test_request("s1")).unwrap();
        let mut failed = test_request("s2");
        failed.success = false;
        save(&conn, &failed).unwrap();

        let stats = get_request_stats(&conn, None).unwrap();
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.successful_requests, 1);
        assert_eq!(stats.failed_requests, 1);
    }

    #[test]
    fn top_models_works() {
        let conn = setup_db();
        save(&conn, &test_request("m1")).unwrap();
        let mut r2 = test_request("m2");
        r2.model = Some("gpt-4".to_string());
        save(&conn, &r2).unwrap();

        let models = get_top_models(&conn, 10).unwrap();
        assert_eq!(models.len(), 2);
    }

    #[test]
    fn recent_errors_works() {
        let conn = setup_db();
        let mut r = test_request("e1");
        r.success = false;
        r.error_message = Some("rate limited".to_string());
        save(&conn, &r).unwrap();

        let errors = get_recent_errors(&conn, 10).unwrap();
        assert_eq!(errors, vec!["rate limited"]);
    }

    #[test]
    fn delete_orphaned_payloads_works() {
        let conn = setup_db();
        save(&conn, &test_request("keep")).unwrap();
        save_payload(&conn, "keep", Some("{}"), None).unwrap();
        // Temporarily disable FK to insert an orphan payload
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO request_payloads (request_id, request_body) VALUES ('gone', '{}')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        let cleaned = delete_orphaned_payloads(&conn).unwrap();
        assert_eq!(cleaned, 1);
        assert!(get_payload(&conn, "keep").unwrap().is_some());
    }
}
