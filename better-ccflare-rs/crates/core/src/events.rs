//! Typed events for the broadcast event bus.
//!
//! Events are broadcast to SSE clients for real-time dashboard updates.
//! All event types serialize to JSON for the SSE `data:` field.

use serde::{Deserialize, Serialize};

/// Default broadcast channel capacity.
pub const EVENT_BUS_CAPACITY: usize = 1024;

/// Token usage breakdown included in request summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// Top-level event enum sent over the broadcast channel.
///
/// Each variant is tagged with `"type"` for JSON serialization so SSE
/// clients can dispatch on the event kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Emitted when a proxy request starts.
    RequestStart {
        id: String,
        timestamp: i64,
        method: String,
        path: String,
        account_id: Option<String>,
        status_code: u16,
        agent_used: Option<String>,
    },

    /// Emitted when a proxy request completes with summary data.
    RequestSummary {
        id: String,
        status: u16,
        tokens: TokenUsage,
        cost: f64,
        duration_ms: u64,
        account_id: Option<String>,
        model: Option<String>,
    },

    /// Emitted for real-time log entries displayed in the dashboard.
    LogEntry {
        level: String,
        message: String,
        timestamp: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fields: Option<serde_json::Value>,
    },
}

impl Event {
    /// Serialize this event to a JSON string for the SSE `data:` field.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Convenience: the SSE event name derived from the variant.
    pub fn event_name(&self) -> &'static str {
        match self {
            Event::RequestStart { .. } => "request_start",
            Event::RequestSummary { .. } => "request_summary",
            Event::LogEntry { .. } => "log_entry",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serializes_with_type_tag() {
        let event = Event::RequestStart {
            id: "abc-123".into(),
            timestamp: 1700000000000,
            method: "POST".into(),
            path: "/v1/messages".into(),
            account_id: Some("acct-1".into()),
            status_code: 200,
            agent_used: None,
        };
        let json = event.to_json().unwrap();
        assert!(json.contains(r#""type":"request_start""#));
        assert!(json.contains(r#""id":"abc-123""#));
    }

    #[test]
    fn request_summary_serializes() {
        let event = Event::RequestSummary {
            id: "req-1".into(),
            status: 200,
            tokens: TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(50),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
            cost: 0.0015,
            duration_ms: 1200,
            account_id: Some("acct-1".into()),
            model: Some("claude-sonnet-4-5-20250929".into()),
        };
        let json = event.to_json().unwrap();
        assert!(json.contains(r#""type":"request_summary""#));
        assert!(json.contains(r#""input_tokens":100"#));
    }

    #[test]
    fn log_entry_serializes() {
        let event = Event::LogEntry {
            level: "info".into(),
            message: "Request completed".into(),
            timestamp: 1700000000000,
            fields: Some(serde_json::json!({"account": "acct-1"})),
        };
        let json = event.to_json().unwrap();
        assert!(json.contains(r#""type":"log_entry""#));
        assert!(json.contains(r#""level":"info""#));
    }

    #[test]
    fn log_entry_skips_none_fields() {
        let event = Event::LogEntry {
            level: "warn".into(),
            message: "test".into(),
            timestamp: 0,
            fields: None,
        };
        let json = event.to_json().unwrap();
        assert!(!json.contains("fields"));
    }

    #[test]
    fn event_name_returns_correct_name() {
        let start = Event::RequestStart {
            id: String::new(),
            timestamp: 0,
            method: String::new(),
            path: String::new(),
            account_id: None,
            status_code: 0,
            agent_used: None,
        };
        assert_eq!(start.event_name(), "request_start");

        let summary = Event::RequestSummary {
            id: String::new(),
            status: 0,
            tokens: TokenUsage {
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            },
            cost: 0.0,
            duration_ms: 0,
            account_id: None,
            model: None,
        };
        assert_eq!(summary.event_name(), "request_summary");
    }

    #[test]
    fn event_deserializes_from_json() {
        let json = r#"{"type":"request_start","id":"x","timestamp":0,"method":"GET","path":"/","account_id":null,"status_code":200,"agent_used":null}"#;
        let event: Event = serde_json::from_str(json).unwrap();
        match event {
            Event::RequestStart {
                id, status_code, ..
            } => {
                assert_eq!(id, "x");
                assert_eq!(status_code, 200);
            }
            _ => panic!("expected RequestStart"),
        }
    }
}
