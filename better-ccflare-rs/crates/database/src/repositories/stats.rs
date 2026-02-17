//! Stats repository — aggregated analytics queries.
//!
//! Matches the TypeScript `StatsRepository` consolidation.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::error::DbError;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Aggregated request statistics.
#[derive(Debug, Clone)]
pub struct AggregatedStats {
    pub total_requests: i64,
    pub successful_requests: i64,
    pub avg_response_time: f64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub avg_tokens_per_second: Option<f64>,
}

/// Per-account statistics.
#[derive(Debug, Clone)]
pub struct AccountStats {
    pub name: String,
    pub request_count: i64,
    pub success_rate: f64,
    pub total_requests: i64,
}

/// Top model entry with percentage.
#[derive(Debug, Clone)]
pub struct TopModel {
    pub model: String,
    pub count: i64,
    pub percentage: f64,
}

/// API key usage statistics.
#[derive(Debug, Clone)]
pub struct ApiKeyStats {
    pub id: String,
    pub name: String,
    pub requests: i64,
    pub success_rate: f64,
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Get aggregated statistics across all requests.
pub fn get_aggregated_stats(conn: &Connection) -> Result<AggregatedStats, DbError> {
    let result = conn.query_row(
        "SELECT
            COUNT(*) as total_requests,
            SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as successful_requests,
            AVG(response_time_ms) as avg_response_time,
            SUM(input_tokens) as input_tokens,
            SUM(output_tokens) as output_tokens,
            SUM(cache_creation_input_tokens) as cache_creation_input_tokens,
            SUM(cache_read_input_tokens) as cache_read_input_tokens,
            SUM(cost_usd) as total_cost_usd,
            AVG(tokens_per_second) as avg_tokens_per_second
         FROM requests",
        [],
        |row| {
            let input: i64 = row.get::<_, Option<i64>>(3)?.unwrap_or(0);
            let output: i64 = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
            let cache_create: i64 = row.get::<_, Option<i64>>(5)?.unwrap_or(0);
            let cache_read: i64 = row.get::<_, Option<i64>>(6)?.unwrap_or(0);
            let total_tokens = input + output + cache_create + cache_read;

            Ok(AggregatedStats {
                total_requests: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                successful_requests: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                avg_response_time: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                total_tokens,
                total_cost_usd: row.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                input_tokens: input,
                output_tokens: output,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: cache_create,
                avg_tokens_per_second: row.get(8)?,
            })
        },
    )?;
    Ok(result)
}

/// Get per-account statistics with success rates.
pub fn get_account_stats(conn: &Connection, limit: i64) -> Result<Vec<AccountStats>, DbError> {
    // Get account request counts from the accounts table
    let mut stmt = conn.prepare(
        "SELECT
            a.id,
            a.name,
            a.request_count as request_count,
            a.total_requests as total_requests
         FROM accounts a
         WHERE a.request_count > 0
         ORDER BY a.request_count DESC
         LIMIT ?1",
    )?;

    let accounts: Vec<(String, String, i64, i64)> = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if accounts.is_empty() {
        return Ok(Vec::new());
    }

    // Build success rate lookup for these accounts
    let mut result = Vec::with_capacity(accounts.len());
    for (id, name, req_count, total) in &accounts {
        let success_rate: f64 = conn
            .query_row(
                "SELECT
                    CASE WHEN COUNT(*) > 0
                        THEN ROUND(CAST(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) * 100, 2)
                        ELSE 0.0
                    END
                 FROM requests
                 WHERE account_used = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        result.push(AccountStats {
            name: name.clone(),
            request_count: *req_count,
            success_rate,
            total_requests: *total,
        });
    }

    Ok(result)
}

/// Count of accounts that have been used at least once.
pub fn get_active_account_count(conn: &Connection) -> Result<i64, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE request_count > 0",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Distinct error messages, most recent first.
pub fn get_recent_errors(conn: &Connection, limit: i64) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT error_message
         FROM requests
         WHERE error_message IS NOT NULL AND error_message != ''
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

/// Distinct project names.
pub fn get_distinct_projects(conn: &Connection, limit: i64) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT project
         FROM requests
         WHERE project IS NOT NULL AND project != ''
         ORDER BY project
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Top models with percentage.
pub fn get_top_models(conn: &Connection, limit: i64) -> Result<Vec<TopModel>, DbError> {
    let mut stmt = conn.prepare(
        "WITH model_counts AS (
            SELECT model, COUNT(*) as count
            FROM requests
            WHERE model IS NOT NULL
            GROUP BY model
         ),
         total AS (
            SELECT COUNT(*) as total FROM requests WHERE model IS NOT NULL
         )
         SELECT
            mc.model,
            mc.count,
            ROUND(CAST(mc.count AS REAL) / t.total * 100, 2) as percentage
         FROM model_counts mc, total t
         ORDER BY mc.count DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(TopModel {
            model: row.get(0)?,
            count: row.get(1)?,
            percentage: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// API key usage statistics with success rates.
pub fn get_api_key_stats(conn: &Connection) -> Result<Vec<ApiKeyStats>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT
            api_key_id as id,
            api_key_name as name,
            COUNT(*) as requests
         FROM requests
         WHERE api_key_id IS NOT NULL
         GROUP BY api_key_id, api_key_name
         HAVING requests > 0
         ORDER BY requests DESC",
    )?;
    let keys: Vec<(String, String, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut result = Vec::with_capacity(keys.len());
    for (id, name, requests) in &keys {
        let success_rate: f64 = conn
            .query_row(
                "SELECT
                    CASE WHEN COUNT(*) > 0
                        THEN ROUND(CAST(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS REAL) / COUNT(*) * 100, 2)
                        ELSE 0.0
                    END
                 FROM requests
                 WHERE api_key_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        result.push(ApiKeyStats {
            id: id.clone(),
            name: name.clone(),
            requests: *requests,
            success_rate,
        });
    }

    Ok(result)
}

/// Per-account usage across 5h, 24h, and 7d time windows.
#[derive(Debug, Clone, Default)]
pub struct AccountUsageWindows {
    pub account_id: String,
    pub requests_5h: i64,
    pub tokens_5h: i64,
    pub cost_5h: f64,
    pub requests_24h: i64,
    pub tokens_24h: i64,
    pub cost_24h: f64,
    pub requests_7d: i64,
    pub tokens_7d: i64,
    pub cost_7d: f64,
}

/// Get usage windows (5h, 24h, 7d) for all accounts in a single query.
pub fn get_all_account_usage_windows(
    conn: &Connection,
) -> Result<HashMap<String, AccountUsageWindows>, DbError> {
    let now = chrono::Utc::now().timestamp_millis();
    let t_5h = now - 5 * 60 * 60 * 1000;
    let t_24h = now - 24 * 60 * 60 * 1000;
    let t_7d = now - 7 * 24 * 60 * 60 * 1000;

    let mut stmt = conn.prepare(
        "SELECT account_used,
            SUM(CASE WHEN timestamp >= ?1 THEN 1 ELSE 0 END),
            SUM(CASE WHEN timestamp >= ?1 THEN COALESCE(input_tokens,0)+COALESCE(output_tokens,0)+COALESCE(cache_read_input_tokens,0)+COALESCE(cache_creation_input_tokens,0) ELSE 0 END),
            SUM(CASE WHEN timestamp >= ?1 THEN COALESCE(cost_usd,0) ELSE 0.0 END),
            SUM(CASE WHEN timestamp >= ?2 THEN 1 ELSE 0 END),
            SUM(CASE WHEN timestamp >= ?2 THEN COALESCE(input_tokens,0)+COALESCE(output_tokens,0)+COALESCE(cache_read_input_tokens,0)+COALESCE(cache_creation_input_tokens,0) ELSE 0 END),
            SUM(CASE WHEN timestamp >= ?2 THEN COALESCE(cost_usd,0) ELSE 0.0 END),
            COUNT(*),
            SUM(COALESCE(input_tokens,0)+COALESCE(output_tokens,0)+COALESCE(cache_read_input_tokens,0)+COALESCE(cache_creation_input_tokens,0)),
            SUM(COALESCE(cost_usd,0))
         FROM requests
         WHERE timestamp >= ?3 AND account_used IS NOT NULL
         GROUP BY account_used",
    )?;

    let rows = stmt.query_map(params![t_5h, t_24h, t_7d], |row| {
        Ok(AccountUsageWindows {
            account_id: row.get::<_, String>(0)?,
            requests_5h: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            tokens_5h: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            cost_5h: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            requests_24h: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            tokens_24h: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            cost_24h: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            requests_7d: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
            tokens_7d: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
            cost_7d: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
        })
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let usage = row?;
        map.insert(usage.account_id.clone(), usage);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{account, request};
    use crate::schema;
    use bccf_core::types::{Account, ProxyRequest};

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
            refresh_token: "rt".to_string(),
            access_token: Some("at".to_string()),
            expires_at: Some(9999999999999),
            request_count: 5,
            total_requests: 10,
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

    fn test_request(id: &str, account_id: &str, success: bool) -> ProxyRequest {
        ProxyRequest {
            id: id.to_string(),
            timestamp: 1700000000000,
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            account_used: Some(account_id.to_string()),
            status_code: Some(if success { 200 } else { 500 }),
            success,
            error_message: if success {
                None
            } else {
                Some("error".to_string())
            },
            response_time_ms: Some(100),
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
            tokens_per_second: Some(33.0),
            project: Some("proj".to_string()),
            api_key_id: None,
            api_key_name: None,
        }
    }

    #[test]
    fn aggregated_stats_empty() {
        let conn = setup_db();
        let stats = get_aggregated_stats(&conn).unwrap();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.total_cost_usd, 0.0);
    }

    #[test]
    fn aggregated_stats_with_data() {
        let conn = setup_db();
        request::save(&conn, &test_request("r1", "a1", true)).unwrap();
        request::save(&conn, &test_request("r2", "a1", false)).unwrap();

        let stats = get_aggregated_stats(&conn).unwrap();
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.successful_requests, 1);
        assert!(stats.total_cost_usd > 0.0);
    }

    #[test]
    fn account_stats_works() {
        let conn = setup_db();
        account::create(&conn, &test_account("a1", "Account 1")).unwrap();
        request::save(&conn, &test_request("r1", "a1", true)).unwrap();
        request::save(&conn, &test_request("r2", "a1", false)).unwrap();

        let stats = get_account_stats(&conn, 10).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].name, "Account 1");
        assert_eq!(stats[0].success_rate, 50.0);
    }

    #[test]
    fn active_account_count() {
        let conn = setup_db();
        account::create(&conn, &test_account("a1", "Active")).unwrap();
        let mut inactive = test_account("a2", "Inactive");
        inactive.request_count = 0;
        account::create(&conn, &inactive).unwrap();

        let count = get_active_account_count(&conn).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn distinct_projects() {
        let conn = setup_db();
        let mut r1 = test_request("r1", "a1", true);
        r1.project = Some("alpha".to_string());
        let mut r2 = test_request("r2", "a1", true);
        r2.project = Some("beta".to_string());
        request::save(&conn, &r1).unwrap();
        request::save(&conn, &r2).unwrap();

        let projects = get_distinct_projects(&conn, 50).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(projects.contains(&"alpha".to_string()));
    }

    #[test]
    fn top_models_with_percentage() {
        let conn = setup_db();
        request::save(&conn, &test_request("r1", "a1", true)).unwrap();
        let mut r2 = test_request("r2", "a1", true);
        r2.model = Some("gpt-4".to_string());
        request::save(&conn, &r2).unwrap();

        let models = get_top_models(&conn, 10).unwrap();
        assert_eq!(models.len(), 2);
        // Each model has 50%
        assert!((models[0].percentage - 50.0).abs() < 0.01);
    }

    #[test]
    fn account_usage_windows_empty() {
        let conn = setup_db();
        let windows = get_all_account_usage_windows(&conn).unwrap();
        assert!(windows.is_empty());
    }

    #[test]
    fn account_usage_windows_with_data() {
        let conn = setup_db();
        let now = chrono::Utc::now().timestamp_millis();
        let mut r1 = test_request("r1", "a1", true);
        r1.timestamp = now - 1000; // 1 second ago (within 5h)
        request::save(&conn, &r1).unwrap();

        let windows = get_all_account_usage_windows(&conn).unwrap();
        assert_eq!(windows.len(), 1);
        let w = windows.get("a1").unwrap();
        assert_eq!(w.requests_5h, 1);
        assert_eq!(w.requests_24h, 1);
        assert_eq!(w.requests_7d, 1);
        assert!(w.tokens_5h > 0);
    }
}
