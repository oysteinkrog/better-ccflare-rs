use std::collections::HashMap;

// Full model IDs as used by the Anthropic API.
pub const HAIKU_3_5: &str = "claude-3-5-haiku-20241022";
pub const SONNET_3_5: &str = "claude-3-5-sonnet-20241022";
pub const SONNET_4: &str = "claude-sonnet-4-20250514";
pub const SONNET_4_5: &str = "claude-sonnet-4-5-20250929";
pub const HAIKU_4_5: &str = "claude-haiku-4-5-20251001";
pub const OPUS_4: &str = "claude-opus-4-20250514";
pub const OPUS_4_1: &str = "claude-opus-4-1-20250805";
pub const OPUS_4_5: &str = "claude-opus-4-5-20251101";
pub const OPUS_3: &str = "claude-3-opus-20240229";
pub const SONNET_3: &str = "claude-3-sonnet-20240229";

pub const DEFAULT_MODEL: &str = SONNET_4_5;
pub const DEFAULT_AGENT_MODEL: &str = SONNET_4_5;

/// All known model IDs.
pub const ALL_MODEL_IDS: &[&str] = &[
    HAIKU_3_5, SONNET_3_5, SONNET_4, SONNET_4_5, HAIKU_4_5, OPUS_4, OPUS_4_1, OPUS_4_5, OPUS_3,
    SONNET_3,
];

/// Get a human-readable display name for a model ID.
pub fn get_model_display_name(model_id: &str) -> &str {
    match model_id {
        HAIKU_3_5 => "Claude Haiku 3.5",
        SONNET_3_5 => "Claude Sonnet 3.5 v2",
        SONNET_4 => "Claude Sonnet 4",
        SONNET_4_5 => "Claude Sonnet 4.5",
        HAIKU_4_5 => "Claude Haiku 4.5",
        OPUS_4 => "Claude Opus 4",
        OPUS_4_1 => "Claude Opus 4.1",
        OPUS_4_5 => "Claude Opus 4.5",
        OPUS_3 => "Claude Opus 3",
        SONNET_3 => "Claude Sonnet 3",
        other => other,
    }
}

/// Get a short name for a model ID (used in UI color mapping, etc.).
pub fn get_model_short_name(model_id: &str) -> &str {
    match model_id {
        HAIKU_3_5 => "claude-3.5-haiku",
        SONNET_3_5 => "claude-3.5-sonnet",
        SONNET_4 => "claude-sonnet-4",
        SONNET_4_5 => "claude-sonnet-4.5",
        HAIKU_4_5 => "claude-haiku-4.5",
        OPUS_4 => "claude-opus-4",
        OPUS_4_1 => "claude-opus-4.1",
        OPUS_4_5 => "claude-opus-4.5",
        OPUS_3 => "claude-3-opus",
        SONNET_3 => "claude-3-sonnet",
        other => other,
    }
}

/// Check if a string is a valid model ID.
pub fn is_valid_model_id(model_id: &str) -> bool {
    ALL_MODEL_IDS.contains(&model_id)
}

// ---------------------------------------------------------------------------
// Model mappings for OpenAI-compatible providers
// ---------------------------------------------------------------------------

/// Known model family patterns (checked in order: opus, haiku, sonnet).
pub const KNOWN_PATTERNS: &[&str] = &["opus", "haiku", "sonnet"];

/// Default model mappings for OpenAI-compatible providers.
pub fn default_model_mappings() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("opus".into(), "openai/gpt-5".into());
    m.insert("sonnet".into(), "openai/gpt-5".into());
    m.insert("haiku".into(), "openai/gpt-5-mini".into());
    m
}

/// Map an Anthropic model name to a provider-specific name using the given mappings.
pub fn map_model_name(anthropic_model: &str, mappings: &HashMap<String, String>) -> String {
    // Exact match first
    if let Some(mapped) = mappings.get(anthropic_model) {
        return mapped.clone();
    }

    // Family pattern matching
    let normalized = anthropic_model.to_lowercase();
    for pattern in KNOWN_PATTERNS {
        if normalized.contains(pattern) {
            if let Some(mapped) = mappings.get(*pattern) {
                return mapped.clone();
            }
        }
    }

    // Fallback to sonnet mapping
    mappings
        .get("sonnet")
        .cloned()
        .unwrap_or_else(|| "openai/gpt-5".into())
}

/// Parse a JSON string of model mappings.
pub fn parse_model_mappings(json_str: &str) -> Option<HashMap<String, String>> {
    serde_json::from_str::<HashMap<String, String>>(json_str).ok()
}

/// Parse custom endpoint data (either plain URL or JSON with endpoint + modelMappings).
pub fn parse_custom_endpoint_data(
    custom_endpoint: &str,
) -> (Option<String>, Option<HashMap<String, String>>) {
    let trimmed = custom_endpoint.trim();
    if trimmed.is_empty() {
        return (None, None);
    }

    if !trimmed.starts_with('{') {
        return (Some(trimmed.to_string()), None);
    }

    #[derive(serde::Deserialize)]
    struct EndpointData {
        endpoint: Option<String>,
        #[serde(rename = "modelMappings")]
        model_mappings: Option<HashMap<String, String>>,
    }

    match serde_json::from_str::<EndpointData>(trimmed) {
        Ok(data) => (data.endpoint, data.model_mappings),
        Err(_) => (Some(trimmed.to_string()), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_known() {
        assert_eq!(get_model_display_name(SONNET_4_5), "Claude Sonnet 4.5");
    }

    #[test]
    fn display_name_unknown() {
        assert_eq!(get_model_display_name("unknown-model"), "unknown-model");
    }

    #[test]
    fn short_name() {
        assert_eq!(get_model_short_name(OPUS_4), "claude-opus-4");
    }

    #[test]
    fn valid_model_id() {
        assert!(is_valid_model_id(SONNET_4_5));
        assert!(!is_valid_model_id("gpt-4"));
    }

    #[test]
    fn map_model_exact_match() {
        let mut mappings = default_model_mappings();
        mappings.insert(SONNET_4_5.into(), "custom-model".into());
        assert_eq!(map_model_name(SONNET_4_5, &mappings), "custom-model");
    }

    #[test]
    fn map_model_pattern_match() {
        let mappings = default_model_mappings();
        assert_eq!(
            map_model_name("claude-opus-4-20250514", &mappings),
            "openai/gpt-5"
        );
        assert_eq!(
            map_model_name("claude-3-5-haiku-20241022", &mappings),
            "openai/gpt-5-mini"
        );
    }

    #[test]
    fn map_model_fallback() {
        let mappings = default_model_mappings();
        assert_eq!(map_model_name("totally-unknown", &mappings), "openai/gpt-5");
    }

    #[test]
    fn parse_custom_endpoint_plain_url() {
        let (ep, mm) = parse_custom_endpoint_data("https://example.com/api");
        assert_eq!(ep, Some("https://example.com/api".into()));
        assert!(mm.is_none());
    }

    #[test]
    fn parse_custom_endpoint_json() {
        let json = r#"{"endpoint":"https://api.example.com","modelMappings":{"opus":"gpt-5"}}"#;
        let (ep, mm) = parse_custom_endpoint_data(json);
        assert_eq!(ep, Some("https://api.example.com".into()));
        assert_eq!(mm.unwrap().get("opus").unwrap(), "gpt-5");
    }

    #[test]
    fn parse_custom_endpoint_empty() {
        let (ep, mm) = parse_custom_endpoint_data("");
        assert!(ep.is_none());
        assert!(mm.is_none());
    }
}
