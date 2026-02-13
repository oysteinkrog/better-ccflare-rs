//! Analytics API handler.
//!
//! `GET /api/analytics?range=24h&mode=normal` — time-series data with filters.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use bccf_core::AppState;
use bccf_database::DbPool;

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    #[serde(default = "default_range")]
    pub range: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(rename = "modelBreakdown")]
    pub model_breakdown: Option<String>,
    pub accounts: Option<String>,
    pub models: Option<String>,
    #[serde(rename = "apiKeys")]
    pub api_keys: Option<String>,
    pub status: Option<String>,
    pub projects: Option<String>,
}

fn default_range() -> String {
    "24h".to_string()
}
fn default_mode() -> String {
    "normal".to_string()
}

// ---------------------------------------------------------------------------
// Range config
// ---------------------------------------------------------------------------

struct BucketConfig {
    bucket_ms: i64,
    display_name: &'static str,
}

fn get_range_config(range: &str) -> (i64, BucketConfig) {
    let now = chrono::Utc::now().timestamp_millis();
    let hour: i64 = 60 * 60 * 1000;
    let day: i64 = 24 * hour;

    match range {
        "1h" => (
            now - hour,
            BucketConfig {
                bucket_ms: 60_000,
                display_name: "1m",
            },
        ),
        "6h" => (
            now - 6 * hour,
            BucketConfig {
                bucket_ms: 5 * 60_000,
                display_name: "5m",
            },
        ),
        "7d" => (
            now - 7 * day,
            BucketConfig {
                bucket_ms: hour,
                display_name: "1h",
            },
        ),
        "30d" => (
            now - 30 * day,
            BucketConfig {
                bucket_ms: day,
                display_name: "1d",
            },
        ),
        _ => (
            // Default: 24h
            now - day,
            BucketConfig {
                bucket_ms: hour,
                display_name: "1h",
            },
        ),
    }
}

// ---------------------------------------------------------------------------
// Filter builder
// ---------------------------------------------------------------------------

struct FilterBuilder {
    conditions: Vec<String>,
    params: Vec<Box<dyn rusqlite::types::ToSql>>,
    idx: usize,
}

impl FilterBuilder {
    fn new(start_ms: i64) -> Self {
        Self {
            conditions: vec!["timestamp > ?1".to_string()],
            params: vec![Box::new(start_ms)],
            idx: 2,
        }
    }

    fn add_in_filter(&mut self, column: &str, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let placeholders: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", self.idx + i))
            .collect();
        self.conditions
            .push(format!("{column} IN ({})", placeholders.join(",")));
        for v in values {
            self.params.push(Box::new(v.clone()));
        }
        self.idx += values.len();
    }

    fn add_account_filter(&mut self, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let placeholders: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", self.idx + i))
            .collect();
        self.conditions.push(format!(
            "r.account_used IN (SELECT id FROM accounts WHERE name IN ({}))",
            placeholders.join(",")
        ));
        for v in values {
            self.params.push(Box::new(v.clone()));
        }
        self.idx += values.len();
    }

    fn add_status_filter(&mut self, status: &str) {
        match status {
            "success" => self.conditions.push("success = 1".to_string()),
            "error" => self.conditions.push("success = 0".to_string()),
            _ => {}
        }
    }

    fn where_clause(&self) -> String {
        self.conditions.join(" AND ")
    }
}

// ---------------------------------------------------------------------------
// GET /api/analytics
// ---------------------------------------------------------------------------

/// Time-series analytics with configurable ranges and filters.
pub async fn get_analytics(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AnalyticsQuery>,
) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not available"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database connection failed"})),
        )
            .into_response();
    };

    let (start_ms, bucket) = get_range_config(&query.range);
    let is_cumulative = query.mode == "cumulative";
    let include_model_breakdown = query.model_breakdown.as_deref() == Some("true");

    // Build filters
    let mut fb = FilterBuilder::new(start_ms);

    if let Some(ref accounts) = query.accounts {
        let vals: Vec<String> = accounts
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        fb.add_account_filter(&vals);
    }
    if let Some(ref models) = query.models {
        let vals: Vec<String> = models
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        fb.add_in_filter("model", &vals);
    }
    if let Some(ref api_keys) = query.api_keys {
        let vals: Vec<String> = api_keys
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        fb.add_in_filter("api_key_name", &vals);
    }
    if let Some(ref status) = query.status {
        fb.add_status_filter(status);
    }
    if let Some(ref projects) = query.projects {
        let vals: Vec<String> = projects
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        fb.add_in_filter("project", &vals);
    }

    let where_clause = fb.where_clause();

    let result = (|| -> Result<serde_json::Value, rusqlite::Error> {
        // ---------------------------------------------------------------
        // Totals query
        // ---------------------------------------------------------------
        let totals_sql = format!(
            "SELECT
                COUNT(*) as total_requests,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(*), 0) as success_rate,
                AVG(response_time_ms) as avg_response_time,
                SUM(COALESCE(total_tokens, 0)) as total_tokens,
                SUM(COALESCE(cost_usd, 0)) as total_cost_usd,
                AVG(tokens_per_second) as avg_tokens_per_second,
                COUNT(DISTINCT account_used) as active_accounts,
                SUM(COALESCE(input_tokens, 0)) as input_tokens,
                SUM(COALESCE(cache_read_input_tokens, 0)) as cache_read_input_tokens,
                SUM(COALESCE(cache_creation_input_tokens, 0)) as cache_creation_input_tokens,
                SUM(COALESCE(output_tokens, 0)) as output_tokens
             FROM requests r
             WHERE {where_clause}"
        );

        let totals = conn.query_row(
            &totals_sql,
            rusqlite::params_from_iter(fb.params.iter()),
            |row| {
                Ok(json!({
                    "requests": row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    "successRate": row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    "avgResponseTime": row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    "totalTokens": row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "totalCostUsd": row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    "avgTokensPerSecond": row.get::<_, Option<f64>>(5)?,
                    "activeAccounts": row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                }))
            },
        )?;

        let token_breakdown = conn.query_row(
            &totals_sql,
            rusqlite::params_from_iter(fb.params.iter()),
            |row| {
                Ok(json!({
                    "inputTokens": row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    "cacheReadInputTokens": row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                    "cacheCreationInputTokens": row.get::<_, Option<i64>>(9)?.unwrap_or(0),
                    "outputTokens": row.get::<_, Option<i64>>(10)?.unwrap_or(0),
                }))
            },
        )?;

        // ---------------------------------------------------------------
        // Time series
        // ---------------------------------------------------------------
        let model_group = if include_model_breakdown {
            ", model"
        } else {
            ""
        };
        let model_where = if include_model_breakdown {
            " AND model IS NOT NULL"
        } else {
            ""
        };
        let model_select = if include_model_breakdown {
            "model,"
        } else {
            ""
        };

        let bucket_idx = fb.idx;
        let ts_sql = format!(
            "SELECT
                (timestamp / ?{bucket_idx}) * ?{bucket_idx} as ts,
                {model_select}
                COUNT(*) as requests,
                SUM(COALESCE(total_tokens, 0)) as tokens,
                SUM(COALESCE(cost_usd, 0)) as cost_usd,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(*), 0) as success_rate,
                SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(*), 0) as error_rate,
                SUM(COALESCE(cache_read_input_tokens, 0)) * 100.0 /
                    NULLIF(SUM(COALESCE(input_tokens, 0) + COALESCE(cache_read_input_tokens, 0) + COALESCE(cache_creation_input_tokens, 0)), 0) as cache_hit_rate,
                AVG(response_time_ms) as avg_response_time,
                AVG(tokens_per_second) as avg_tokens_per_second
             FROM requests r
             WHERE {where_clause}{model_where}
             GROUP BY ts{model_group}
             ORDER BY ts{model_group}"
        );

        let mut ts_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        // Re-add all filter params first
        ts_params.push(Box::new(start_ms));
        for p in fb.params.iter().skip(1) {
            // We need to re-create params; copy-safe approach
            // The params are String or i64 — use a helper
            ts_params.push(clone_param(p));
        }
        ts_params.push(Box::new(bucket.bucket_ms));

        let mut ts_stmt = conn.prepare(&ts_sql)?;
        let ts_rows = ts_stmt.query_map(rusqlite::params_from_iter(ts_params.iter()), |row| {
            let mut point = json!({
                "ts": row.get::<_, i64>(0)?,
            });

            let map = point.as_object_mut().unwrap();
            let col_offset = if include_model_breakdown {
                map.insert("model".to_string(), json!(row.get::<_, Option<String>>(1)?));
                2
            } else {
                1
            };

            map.insert(
                "requests".to_string(),
                json!(row.get::<_, i64>(col_offset)?),
            );
            map.insert(
                "tokens".to_string(),
                json!(row.get::<_, Option<i64>>(col_offset + 1)?.unwrap_or(0)),
            );
            map.insert(
                "costUsd".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 2)?.unwrap_or(0.0)),
            );
            map.insert(
                "successRate".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 3)?.unwrap_or(0.0)),
            );
            map.insert(
                "errorRate".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 4)?.unwrap_or(0.0)),
            );
            map.insert(
                "cacheHitRate".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 5)?.unwrap_or(0.0)),
            );
            map.insert(
                "avgResponseTime".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 6)?.unwrap_or(0.0)),
            );
            map.insert(
                "avgTokensPerSecond".to_string(),
                json!(row.get::<_, Option<f64>>(col_offset + 7)?),
            );

            Ok(point)
        })?;

        let mut time_series: Vec<serde_json::Value> = Vec::new();
        for row in ts_rows {
            time_series.push(row?);
        }

        // Apply cumulative transformation
        if is_cumulative {
            apply_cumulative(&mut time_series, include_model_breakdown);
        }

        // ---------------------------------------------------------------
        // Model distribution
        // ---------------------------------------------------------------
        let md_sql = format!(
            "SELECT model, COUNT(*) as count
             FROM requests r
             WHERE {where_clause} AND model IS NOT NULL
             GROUP BY model
             ORDER BY count DESC
             LIMIT 10"
        );
        let mut md_stmt = conn.prepare(&md_sql)?;
        let model_distribution: Vec<serde_json::Value> = md_stmt
            .query_map(rusqlite::params_from_iter(fb.params.iter()), |row| {
                Ok(json!({
                    "model": row.get::<_, String>(0)?,
                    "count": row.get::<_, i64>(1)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // ---------------------------------------------------------------
        // Account performance
        // ---------------------------------------------------------------
        let ap_sql = format!(
            "SELECT
                COALESCE(a.name, r.account_used) as name,
                COUNT(r.id) as requests,
                SUM(CASE WHEN r.success = 1 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(r.id), 0) as success_rate
             FROM requests r
             LEFT JOIN accounts a ON a.id = r.account_used
             WHERE {where_clause}
             GROUP BY name
             HAVING requests > 0
             ORDER BY requests DESC
             LIMIT 10"
        );
        let mut ap_stmt = conn.prepare(&ap_sql)?;
        let account_performance: Vec<serde_json::Value> = ap_stmt
            .query_map(rusqlite::params_from_iter(fb.params.iter()), |row| {
                Ok(json!({
                    "name": row.get::<_, Option<String>>(0)?,
                    "requests": row.get::<_, i64>(1)?,
                    "successRate": row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // ---------------------------------------------------------------
        // API key performance
        // ---------------------------------------------------------------
        let ak_sql = format!(
            "SELECT
                api_key_id as id,
                api_key_name as name,
                COUNT(*) as requests,
                SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(*), 0) as success_rate
             FROM requests r
             WHERE {where_clause} AND api_key_id IS NOT NULL
             GROUP BY api_key_id, api_key_name
             HAVING requests > 0
             ORDER BY requests DESC
             LIMIT 10"
        );
        let mut ak_stmt = conn.prepare(&ak_sql)?;
        let api_key_performance: Vec<serde_json::Value> = ak_stmt
            .query_map(rusqlite::params_from_iter(fb.params.iter()), |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "requests": row.get::<_, i64>(2)?,
                    "successRate": row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // ---------------------------------------------------------------
        // Cost by model
        // ---------------------------------------------------------------
        let cbm_sql = format!(
            "SELECT
                model,
                SUM(COALESCE(cost_usd, 0)) as cost_usd,
                COUNT(*) as requests,
                SUM(COALESCE(total_tokens, 0)) as total_tokens
             FROM requests r
             WHERE {where_clause} AND COALESCE(cost_usd, 0) > 0 AND model IS NOT NULL
             GROUP BY model
             ORDER BY cost_usd DESC
             LIMIT 10"
        );
        let mut cbm_stmt = conn.prepare(&cbm_sql)?;
        let cost_by_model: Vec<serde_json::Value> = cbm_stmt
            .query_map(rusqlite::params_from_iter(fb.params.iter()), |row| {
                Ok(json!({
                    "model": row.get::<_, String>(0)?,
                    "costUsd": row.get::<_, f64>(1)?,
                    "requests": row.get::<_, i64>(2)?,
                    "totalTokens": row.get::<_, i64>(3)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // ---------------------------------------------------------------
        // Model performance
        // ---------------------------------------------------------------
        let mp_sql = format!(
            "WITH filtered AS (
                SELECT model, response_time_ms, tokens_per_second, success
                FROM requests r
                WHERE {where_clause} AND model IS NOT NULL AND response_time_ms IS NOT NULL
            ),
            ranked AS (
                SELECT
                    model, response_time_ms, tokens_per_second, success,
                    PERCENT_RANK() OVER (PARTITION BY model ORDER BY response_time_ms) AS pr
                FROM filtered
            )
            SELECT
                model,
                AVG(response_time_ms) as avg_response_time,
                MAX(response_time_ms) as max_response_time,
                COUNT(*) as total_requests,
                SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END) * 100.0 / NULLIF(COUNT(*), 0) as error_rate,
                AVG(tokens_per_second) as avg_tokens_per_second,
                MIN(CASE WHEN pr >= 0.95 THEN response_time_ms END) as p95_response_time,
                MIN(CASE WHEN tokens_per_second > 0 THEN tokens_per_second ELSE NULL END) as min_tokens_per_second,
                MAX(CASE WHEN tokens_per_second > 0 THEN tokens_per_second ELSE NULL END) as max_tokens_per_second
            FROM ranked
            GROUP BY model
            ORDER BY total_requests DESC
            LIMIT 10"
        );
        let mut mp_stmt = conn.prepare(&mp_sql)?;
        let model_performance: Vec<serde_json::Value> = mp_stmt
            .query_map(rusqlite::params_from_iter(fb.params.iter()), |row| {
                let avg_rt: f64 = row.get::<_, Option<f64>>(1)?.unwrap_or(0.0);
                let max_rt: f64 = row.get::<_, Option<f64>>(2)?.unwrap_or(0.0);
                let p95_rt: Option<f64> = row.get(6)?;

                Ok(json!({
                    "model": row.get::<_, String>(0)?,
                    "avgResponseTime": avg_rt,
                    "p95ResponseTime": p95_rt.unwrap_or(max_rt.max(avg_rt)),
                    "errorRate": row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    "avgTokensPerSecond": row.get::<_, Option<f64>>(5)?,
                    "minTokensPerSecond": row.get::<_, Option<f64>>(7)?,
                    "maxTokensPerSecond": row.get::<_, Option<f64>>(8)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // ---------------------------------------------------------------
        // Assemble response
        // ---------------------------------------------------------------
        Ok(json!({
            "meta": {
                "range": query.range,
                "bucket": bucket.display_name,
                "cumulative": is_cumulative,
            },
            "totals": totals,
            "timeSeries": time_series,
            "tokenBreakdown": token_breakdown,
            "modelDistribution": model_distribution,
            "accountPerformance": account_performance,
            "apiKeyPerformance": api_key_performance,
            "costByModel": cost_by_model,
            "modelPerformance": model_performance,
        }))
    })();

    match result {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            error!("Analytics query error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to fetch analytics data"})),
            )
                .into_response()
        }
    }
}

/// Apply cumulative transformation to time series data.
fn apply_cumulative(time_series: &mut [serde_json::Value], model_breakdown: bool) {
    if !model_breakdown {
        let mut running_requests: i64 = 0;
        let mut running_tokens: i64 = 0;
        let mut running_cost: f64 = 0.0;

        for point in time_series.iter_mut() {
            let map = point.as_object_mut().unwrap();
            running_requests += map.get("requests").and_then(|v| v.as_i64()).unwrap_or(0);
            running_tokens += map.get("tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            running_cost += map.get("costUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);

            map.insert("requests".to_string(), json!(running_requests));
            map.insert("tokens".to_string(), json!(running_tokens));
            map.insert("costUsd".to_string(), json!(running_cost));
        }
    } else {
        use std::collections::HashMap;
        let mut totals: HashMap<String, (i64, i64, f64)> = HashMap::new();

        for point in time_series.iter_mut() {
            let map = point.as_object_mut().unwrap();
            let model = map
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let entry = totals.entry(model).or_insert((0, 0, 0.0));
            entry.0 += map.get("requests").and_then(|v| v.as_i64()).unwrap_or(0);
            entry.1 += map.get("tokens").and_then(|v| v.as_i64()).unwrap_or(0);
            entry.2 += map.get("costUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);

            map.insert("requests".to_string(), json!(entry.0));
            map.insert("tokens".to_string(), json!(entry.1));
            map.insert("costUsd".to_string(), json!(entry.2));
        }
    }
}

/// Clone a `Box<dyn ToSql>` for reuse in multiple prepared statements.
///
/// Since we only build filters from `String` and `i64` values, this covers all cases.
fn clone_param(p: &dyn rusqlite::types::ToSql) -> Box<dyn rusqlite::types::ToSql> {
    use rusqlite::types::{ToSqlOutput, ValueRef};

    let output = p.to_sql().unwrap_or(ToSqlOutput::Borrowed(ValueRef::Null));
    match output {
        ToSqlOutput::Borrowed(ValueRef::Integer(i))
        | ToSqlOutput::Owned(rusqlite::types::Value::Integer(i)) => Box::new(i),
        ToSqlOutput::Borrowed(ValueRef::Real(f))
        | ToSqlOutput::Owned(rusqlite::types::Value::Real(f)) => Box::new(f),
        ToSqlOutput::Borrowed(ValueRef::Text(s)) => {
            Box::new(String::from_utf8_lossy(s).to_string())
        }
        ToSqlOutput::Owned(rusqlite::types::Value::Text(s)) => Box::new(s),
        _ => Box::new(rusqlite::types::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_config_1h() {
        let (_, bucket) = get_range_config("1h");
        assert_eq!(bucket.bucket_ms, 60_000);
        assert_eq!(bucket.display_name, "1m");
    }

    #[test]
    fn range_config_24h() {
        let (_, bucket) = get_range_config("24h");
        assert_eq!(bucket.bucket_ms, 3_600_000);
        assert_eq!(bucket.display_name, "1h");
    }

    #[test]
    fn range_config_30d() {
        let (_, bucket) = get_range_config("30d");
        assert_eq!(bucket.bucket_ms, 86_400_000);
        assert_eq!(bucket.display_name, "1d");
    }

    #[test]
    fn range_config_default() {
        let (_, bucket) = get_range_config("invalid");
        assert_eq!(bucket.display_name, "1h");
    }

    #[test]
    fn cumulative_simple() {
        let mut series = vec![
            json!({"ts": 1, "requests": 5, "tokens": 100, "costUsd": 0.1}),
            json!({"ts": 2, "requests": 3, "tokens": 50, "costUsd": 0.05}),
        ];
        apply_cumulative(&mut series, false);
        assert_eq!(series[0]["requests"], 5);
        assert_eq!(series[1]["requests"], 8);
        assert_eq!(series[1]["tokens"], 150);
    }

    #[test]
    fn cumulative_model_breakdown() {
        let mut series = vec![
            json!({"ts": 1, "model": "a", "requests": 5, "tokens": 100, "costUsd": 0.1}),
            json!({"ts": 1, "model": "b", "requests": 2, "tokens": 40, "costUsd": 0.02}),
            json!({"ts": 2, "model": "a", "requests": 3, "tokens": 50, "costUsd": 0.05}),
        ];
        apply_cumulative(&mut series, true);
        assert_eq!(series[0]["requests"], 5);
        assert_eq!(series[1]["requests"], 2);
        assert_eq!(series[2]["requests"], 8); // cumulative for model "a"
    }
}
