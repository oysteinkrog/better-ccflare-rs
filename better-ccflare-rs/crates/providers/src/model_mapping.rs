//! Model name mapping utilities.
//!
//! Translates Anthropic model names to provider-specific names using
//! per-account mappings and known pattern matching.

use bccf_core::types::Account;

/// Known model name patterns for fuzzy matching.
///
/// When an exact mapping isn't found, we try to match against these
/// patterns (case-insensitive substring match).
const KNOWN_PATTERNS: &[&str] = &[
    "sonnet",
    "opus",
    "haiku",
    "claude-3",
    "claude-4",
    "claude-instant",
];

/// Get the mapped model name for the given model, considering account
/// model_mappings configuration.
///
/// Resolution order:
/// 1. If no account or no mappings → return original
/// 2. Try exact match in account.model_mappings
/// 3. Try pattern matching against KNOWN_PATTERNS
/// 4. Return original if no mapping found
pub fn get_model_name(model: &str, account: Option<&Account>) -> String {
    let Some(account) = account else {
        return model.to_string();
    };

    let Some(ref mappings_json) = account.model_mappings else {
        return model.to_string();
    };

    let mappings: serde_json::Value = match serde_json::from_str(mappings_json) {
        Ok(v) => v,
        Err(_) => return model.to_string(),
    };

    let Some(obj) = mappings.as_object() else {
        return model.to_string();
    };

    // 1. Exact match
    if let Some(mapped) = obj.get(model).and_then(|v| v.as_str()) {
        return mapped.to_string();
    }

    // 2. Pattern match: check if any mapping key is a pattern that matches
    let model_lower = model.to_lowercase();
    for (key, value) in obj {
        let key_lower = key.to_lowercase();
        // Check if model contains the pattern key
        if KNOWN_PATTERNS
            .iter()
            .any(|p| key_lower.contains(p) && model_lower.contains(p))
        {
            if let Some(mapped) = value.as_str() {
                return mapped.to_string();
            }
        }
    }

    model.to_string()
}

/// Transform a JSON request body's "model" field using the given mapping function.
///
/// Returns `None` if no transformation was needed, `Some(new_body)` if the
/// model field was changed.
pub fn transform_body_model(body: &[u8], account: Option<&Account>) -> Option<Vec<u8>> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let model = json.get("model")?.as_str()?;
    let mapped = get_model_name(model, account);

    if mapped == model {
        return None; // No change needed
    }

    json["model"] = serde_json::Value::String(mapped);
    serde_json::to_vec(&json).ok()
}

/// Force-replace the model field in a JSON request body with the given model.
///
/// Returns `None` if the body isn't valid JSON with a model field.
pub fn transform_body_model_force(body: &[u8], target_model: &str) -> Option<Vec<u8>> {
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
    if json.get("model").is_none() {
        return None;
    }
    json["model"] = serde_json::Value::String(target_model.to_string());
    serde_json::to_vec(&json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_account_with_mappings;

    #[test]
    fn no_account_returns_original() {
        assert_eq!(get_model_name("claude-3-opus", None), "claude-3-opus");
    }

    #[test]
    fn no_mappings_returns_original() {
        let account = crate::test_util::test_account_with_key("sk-test");
        assert_eq!(
            get_model_name("claude-3-opus", Some(&account)),
            "claude-3-opus"
        );
    }

    #[test]
    fn exact_match() {
        let account = test_account_with_mappings(r#"{"claude-3-opus":"my-custom-opus"}"#);
        assert_eq!(
            get_model_name("claude-3-opus", Some(&account)),
            "my-custom-opus"
        );
    }

    #[test]
    fn pattern_match_opus() {
        let account = test_account_with_mappings(r#"{"opus":"custom-opus-model"}"#);
        assert_eq!(
            get_model_name("claude-3-opus-20240229", Some(&account)),
            "custom-opus-model"
        );
    }

    #[test]
    fn no_match_returns_original() {
        let account = test_account_with_mappings(r#"{"gpt-4":"custom"}"#);
        assert_eq!(
            get_model_name("claude-3-opus", Some(&account)),
            "claude-3-opus"
        );
    }

    #[test]
    fn invalid_json_returns_original() {
        let account = test_account_with_mappings("not json");
        assert_eq!(
            get_model_name("claude-3-opus", Some(&account)),
            "claude-3-opus"
        );
    }

    #[test]
    fn transform_body_with_mapping() {
        let account = test_account_with_mappings(r#"{"claude-3-opus":"mapped-opus"}"#);
        let body = br#"{"model":"claude-3-opus","messages":[]}"#;
        let result = transform_body_model(body, Some(&account)).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(json["model"], "mapped-opus");
        assert!(json.get("messages").is_some()); // preserved other fields
    }

    #[test]
    fn transform_body_no_change() {
        let account = test_account_with_mappings(r#"{"gpt-4":"custom"}"#);
        let body = br#"{"model":"claude-3-opus","messages":[]}"#;
        assert!(transform_body_model(body, Some(&account)).is_none());
    }

    #[test]
    fn force_transform_body() {
        let body = br#"{"model":"claude-3-opus","messages":[]}"#;
        let result = transform_body_model_force(body, "forced-model").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(json["model"], "forced-model");
    }
}
