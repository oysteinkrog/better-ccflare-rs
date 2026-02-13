use crate::errors::AppError;

/// Validate a string value with various constraints.
pub fn validate_string(
    value: Option<&str>,
    field: &str,
    opts: &StringValidationOpts,
) -> Result<Option<String>, AppError> {
    let val = match value {
        Some(v) => v,
        None => {
            if opts.required {
                return Err(AppError::validation_field(
                    format!("{field} is required"),
                    field,
                ));
            }
            return Ok(None);
        }
    };

    let sanitized = if opts.trim { val.trim() } else { val };

    if let Some(min) = opts.min_length {
        if sanitized.len() < min {
            return Err(AppError::validation_field(
                format!("{field} must be at least {min} characters long"),
                field,
            ));
        }
    }

    if let Some(max) = opts.max_length {
        if sanitized.len() > max {
            return Err(AppError::validation_field(
                format!("{field} must be at most {max} characters long"),
                field,
            ));
        }
    }

    if let Some(ref allowed) = opts.allowed_values {
        if !allowed.contains(&sanitized.to_string()) {
            return Err(AppError::validation_field(
                format!("{field} must be one of: {}", allowed.join(", ")),
                field,
            ));
        }
    }

    Ok(Some(sanitized.to_string()))
}

#[derive(Debug, Default)]
pub struct StringValidationOpts {
    pub required: bool,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub allowed_values: Option<Vec<String>>,
    pub trim: bool,
}

/// Validate a numeric value with various constraints.
pub fn validate_number(
    value: Option<f64>,
    field: &str,
    opts: &NumberValidationOpts,
) -> Result<Option<f64>, AppError> {
    let num = match value {
        Some(n) => n,
        None => {
            if opts.required {
                return Err(AppError::validation_field(
                    format!("{field} is required"),
                    field,
                ));
            }
            return Ok(None);
        }
    };

    if opts.integer && num.fract() != 0.0 {
        return Err(AppError::validation_field(
            format!("{field} must be an integer"),
            field,
        ));
    }

    if let Some(min) = opts.min {
        if num < min {
            return Err(AppError::validation_field(
                format!("{field} must be at least {min}"),
                field,
            ));
        }
    }

    if let Some(max) = opts.max {
        if num > max {
            return Err(AppError::validation_field(
                format!("{field} must be at most {max}"),
                field,
            ));
        }
    }

    Ok(Some(num))
}

#[derive(Debug, Default)]
pub struct NumberValidationOpts {
    pub required: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub integer: bool,
}

/// Validate an endpoint URL.
pub fn validate_endpoint_url(url: &str, field: &str) -> Result<String, AppError> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(AppError::validation_field(
            format!("{field} is required"),
            field,
        ));
    }

    // Must start with http:// or https://
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(AppError::validation_field(
            format!("{field} protocol must be http or https"),
            field,
        ));
    }

    // Must have a hostname after the protocol
    let after_protocol = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or_default();

    if after_protocol.is_empty() || after_protocol.starts_with('/') {
        return Err(AppError::validation_field(
            format!("{field} must have a valid hostname"),
            field,
        ));
    }

    Ok(trimmed.to_string())
}

/// Validate an API key format (basic length check).
pub fn validate_api_key(api_key: &str, field: &str) -> Result<String, AppError> {
    let trimmed = api_key.trim();
    if trimmed.len() < 10 {
        return Err(AppError::validation_field(
            format!("{field} must be at least 10 characters"),
            field,
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate account priority (0-100).
pub fn validate_priority(priority: i64) -> Result<i64, AppError> {
    if !(0..=100).contains(&priority) {
        return Err(AppError::validation_field(
            "priority must be between 0 and 100",
            "priority",
        ));
    }
    Ok(priority)
}

/// Regex patterns for validation.
pub mod patterns {
    /// Account name: alphanumeric with spaces, hyphens, and underscores.
    pub fn is_valid_account_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '_')
    }

    /// URL pattern.
    pub fn is_url(s: &str) -> bool {
        s.starts_with("http://") || s.starts_with("https://")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_string_required_missing() {
        let opts = StringValidationOpts {
            required: true,
            ..Default::default()
        };
        assert!(validate_string(None, "name", &opts).is_err());
    }

    #[test]
    fn validate_string_optional_missing() {
        let opts = StringValidationOpts::default();
        assert_eq!(validate_string(None, "name", &opts).unwrap(), None);
    }

    #[test]
    fn validate_string_min_length() {
        let opts = StringValidationOpts {
            min_length: Some(3),
            ..Default::default()
        };
        assert!(validate_string(Some("ab"), "name", &opts).is_err());
        assert!(validate_string(Some("abc"), "name", &opts).is_ok());
    }

    #[test]
    fn validate_string_max_length() {
        let opts = StringValidationOpts {
            max_length: Some(3),
            ..Default::default()
        };
        assert!(validate_string(Some("abcd"), "name", &opts).is_err());
        assert!(validate_string(Some("abc"), "name", &opts).is_ok());
    }

    #[test]
    fn validate_string_allowed_values() {
        let opts = StringValidationOpts {
            allowed_values: Some(vec!["OFF".into(), "NORMAL".into(), "FULL".into()]),
            ..Default::default()
        };
        assert!(validate_string(Some("NORMAL"), "sync", &opts).is_ok());
        assert!(validate_string(Some("WAL"), "sync", &opts).is_err());
    }

    #[test]
    fn validate_number_range() {
        let opts = NumberValidationOpts {
            min: Some(0.0),
            max: Some(100.0),
            integer: true,
            ..Default::default()
        };
        assert!(validate_number(Some(50.0), "priority", &opts).is_ok());
        assert!(validate_number(Some(101.0), "priority", &opts).is_err());
        assert!(validate_number(Some(-1.0), "priority", &opts).is_err());
        assert!(validate_number(Some(1.5), "priority", &opts).is_err());
    }

    #[test]
    fn validate_endpoint_url_valid() {
        assert_eq!(
            validate_endpoint_url("https://api.example.com/", "url").unwrap(),
            "https://api.example.com"
        );
    }

    #[test]
    fn validate_endpoint_url_invalid() {
        assert!(validate_endpoint_url("ftp://example.com", "url").is_err());
        assert!(validate_endpoint_url("", "url").is_err());
    }

    #[test]
    fn validate_api_key_short() {
        assert!(validate_api_key("short", "key").is_err());
        assert!(validate_api_key("sk-ant-api03-long-enough-key", "key").is_ok());
    }

    #[test]
    fn validate_priority_range() {
        assert!(validate_priority(0).is_ok());
        assert!(validate_priority(100).is_ok());
        assert!(validate_priority(-1).is_err());
        assert!(validate_priority(101).is_err());
    }

    #[test]
    fn account_name_pattern() {
        assert!(patterns::is_valid_account_name("my-account_1"));
        assert!(patterns::is_valid_account_name("My Account"));
        assert!(!patterns::is_valid_account_name(""));
        assert!(!patterns::is_valid_account_name("bad@name"));
    }
}
