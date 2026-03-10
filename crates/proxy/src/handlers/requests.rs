//! Request history API handlers.
//!
//! - `GET /api/requests` — paginated request history with filters
//! - `GET /api/requests/:id/payload` — lazy-load request/response payloads

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use bccf_core::AppState;
use bccf_database::repositories::request as request_repo;
use bccf_database::DbPool;

/// Maximum payload preview size (32 KB).
const MAX_BODY_PREVIEW_BYTES: usize = 32 * 1024;

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RequestsQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    pub account: Option<String>,
    pub model: Option<String>,
    pub project: Option<String>,
    pub tokens_only: Option<bool>,
}

fn default_page() -> i64 {
    1
}
fn default_limit() -> i64 {
    50
}

// ---------------------------------------------------------------------------
// GET /api/requests
// ---------------------------------------------------------------------------

/// Paginated request history with optional filters.
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RequestsQuery>,
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

    let page = query.page.max(1);
    let limit = query.limit.clamp(1, 200);
    let offset = (page - 1) * limit;

    // Build dynamic WHERE clause
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(ref acct) = query.account {
        conditions.push(format!(
            "account_used IN (SELECT id FROM accounts WHERE name = ?{idx})"
        ));
        params.push(Box::new(acct.clone()));
        idx += 1;
    }
    if let Some(ref model) = query.model {
        conditions.push(format!("model = ?{idx}"));
        params.push(Box::new(model.clone()));
        idx += 1;
    }
    if let Some(ref project) = query.project {
        conditions.push(format!("project = ?{idx}"));
        params.push(Box::new(project.clone()));
        idx += 1;
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    // Count total matching rows
    let count_sql = format!("SELECT COUNT(*) FROM requests {where_clause}");
    let total: i64 = match conn.query_row(
        &count_sql,
        rusqlite::params_from_iter(params.iter()),
        |row| row.get(0),
    ) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to count requests: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    // Fetch page
    let select_sql = format!(
        "SELECT r.*, COALESCE(a.name, r.account_used) as account_name
         FROM requests r
         LEFT JOIN accounts a ON a.id = r.account_used
         {where_clause}
         ORDER BY r.timestamp DESC
         LIMIT ?{idx} OFFSET ?{}",
        idx + 1
    );
    params.push(Box::new(limit));
    params.push(Box::new(offset));

    let tokens_only = query.tokens_only.unwrap_or(false);

    let result: Result<Vec<serde_json::Value>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare(&select_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let account_name: Option<String> = row.get("account_name")?;
            let mut obj = serde_json::json!({
                "id": row.get::<_, String>("id")?,
                "timestamp": row.get::<_, i64>("timestamp")?,
                "method": row.get::<_, String>("method")?,
                "path": row.get::<_, String>("path")?,
                "accountUsed": row.get::<_, Option<String>>("account_used")?,
                "accountName": account_name,
                "statusCode": row.get::<_, Option<i64>>("status_code")?,
                "success": row.get::<_, i64>("success")? != 0,
                "errorMessage": row.get::<_, Option<String>>("error_message")?,
                "responseTimeMs": row.get::<_, Option<i64>>("response_time_ms")?,
                "failoverAttempts": row.get::<_, Option<i64>>("failover_attempts")?.unwrap_or(0),
                "model": row.get::<_, Option<String>>("model")?,
                "agentUsed": row.get::<_, Option<String>>("agent_used")?,
                "project": row.get::<_, Option<String>>("project")?,
                "apiKeyId": row.get::<_, Option<String>>("api_key_id")?,
                "apiKeyName": row.get::<_, Option<String>>("api_key_name")?,
            });

            if !tokens_only {
                let map = obj.as_object_mut().unwrap();
                map.insert(
                    "promptTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("prompt_tokens")?),
                );
                map.insert(
                    "completionTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("completion_tokens")?),
                );
                map.insert(
                    "totalTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("total_tokens")?),
                );
                map.insert(
                    "costUsd".to_string(),
                    json!(row.get::<_, Option<f64>>("cost_usd")?),
                );
                map.insert(
                    "inputTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("input_tokens")?),
                );
                map.insert(
                    "cacheReadInputTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("cache_read_input_tokens")?),
                );
                map.insert(
                    "cacheCreationInputTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("cache_creation_input_tokens")?),
                );
                map.insert(
                    "outputTokens".to_string(),
                    json!(row.get::<_, Option<i64>>("output_tokens")?),
                );
                map.insert(
                    "tokensPerSecond".to_string(),
                    json!(row.get::<_, Option<f64>>("tokens_per_second")?),
                );
            }

            Ok(obj)
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })();

    match result {
        Ok(requests) => {
            let total_pages = (total + limit - 1) / limit;
            (
                [
                    ("X-Total-Count", total.to_string()),
                    ("X-Total-Pages", total_pages.to_string()),
                ],
                Json(json!(requests)),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to fetch requests: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// GET /api/requests/:id/payload
// ---------------------------------------------------------------------------

/// Lazy-load request/response payload for a specific request.
pub async fn get_request_payload(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<String>,
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

    match request_repo::get_payload(&conn, &request_id) {
        Ok(Some((req_body, res_body))) => {
            let (req_body_out, req_truncated) = truncate_body(req_body.as_deref());
            let (res_body_out, res_truncated) = truncate_body(res_body.as_deref());

            Json(json!({
                "requestId": request_id,
                "requestBody": req_body_out,
                "responseBody": res_body_out,
                "meta": {
                    "requestBodyTruncated": req_truncated,
                    "responseBodyTruncated": res_truncated,
                }
            }))
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Payload not found"})),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to fetch payload: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response()
        }
    }
}

/// Truncate a body string if it exceeds MAX_BODY_PREVIEW_BYTES.
fn truncate_body(body: Option<&str>) -> (Option<String>, bool) {
    match body {
        None => (None, false),
        Some(s) => {
            if s.len() <= MAX_BODY_PREVIEW_BYTES {
                (Some(s.to_string()), false)
            } else {
                // Find last valid UTF-8 char boundary at or before the limit
                let end = (0..=4)
                    .find_map(|i| {
                        let pos = MAX_BODY_PREVIEW_BYTES.saturating_sub(i);
                        s.is_char_boundary(pos).then_some(pos)
                    })
                    .unwrap_or(0);
                (Some(s[..end].to_string()), true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_none() {
        let (body, truncated) = truncate_body(None);
        assert!(body.is_none());
        assert!(!truncated);
    }

    #[test]
    fn truncate_body_short() {
        let (body, truncated) = truncate_body(Some("hello"));
        assert_eq!(body.as_deref(), Some("hello"));
        assert!(!truncated);
    }

    #[test]
    fn truncate_body_long() {
        let long = "a".repeat(MAX_BODY_PREVIEW_BYTES + 100);
        let (body, truncated) = truncate_body(Some(&long));
        assert_eq!(body.unwrap().len(), MAX_BODY_PREVIEW_BYTES);
        assert!(truncated);
    }
}
