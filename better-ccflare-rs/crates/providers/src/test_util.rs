//! Test utilities for the providers crate.

use bccf_core::types::Account;

/// Create a test account with the given API key.
pub fn test_account_with_key(api_key: &str) -> Account {
    Account {
        id: "test-account".to_string(),
        name: "Test Account".to_string(),
        provider: "stub".to_string(),
        api_key: Some(api_key.to_string()),
        refresh_token: String::new(),
        access_token: None,
        expires_at: None,
        request_count: 0,
        total_requests: 0,
        last_used: None,
        created_at: 0,
        rate_limited_until: None,
        session_start: None,
        session_request_count: 0,
        paused: false,
        rate_limit_reset: None,
        rate_limit_status: None,
        rate_limit_remaining: None,
        priority: 0,
        auto_fallback_enabled: false,
        auto_refresh_enabled: false,
        custom_endpoint: None,
        model_mappings: None,
        reserve_5h: 0,
        reserve_weekly: 0,
        reserve_hard: false,
        subscription_tier: None,
        email: None,
        refresh_token_updated_at: None,
    }
}

/// Create a test account with model mappings JSON.
pub fn test_account_with_mappings(mappings_json: &str) -> Account {
    Account {
        model_mappings: Some(mappings_json.to_string()),
        ..test_account_with_key("sk-test")
    }
}
