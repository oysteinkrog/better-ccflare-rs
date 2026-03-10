use std::path::{Component, Path, PathBuf};

/// Result of path validation.
#[derive(Debug, Clone)]
pub struct PathValidationResult {
    pub is_valid: bool,
    pub resolved_path: PathBuf,
    pub reason: Option<String>,
}

/// Options for path validation.
#[derive(Debug, Clone, Default)]
pub struct PathValidationOptions {
    /// Additional allowed base directories beyond defaults.
    pub additional_allowed_paths: Vec<PathBuf>,
    /// Whether to allow empty paths (default: true).
    pub allow_empty: bool,
}

/// Validate a path for directory traversal and other security issues.
///
/// Checks:
/// 1. Null byte detection
/// 2. Directory traversal ("..") detection
/// 3. Path resolution and whitelist validation
pub fn validate_path(raw_path: &str, options: &PathValidationOptions) -> PathValidationResult {
    let invalid = |reason: String| PathValidationResult {
        is_valid: false,
        resolved_path: PathBuf::new(),
        reason: Some(reason),
    };

    // Check empty
    if raw_path.is_empty() && !options.allow_empty {
        return invalid("Empty path not allowed".into());
    }

    // Null byte check
    if raw_path.contains('\0') {
        return invalid("Null byte detected in path".into());
    }

    // Directory traversal check
    if raw_path.contains("..") {
        return invalid("Directory traversal detected in path".into());
    }

    // Resolve and normalize
    let path = Path::new(raw_path);
    let resolved = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => {
            // If the path exists but canonicalize failed, reject it to avoid symlink TOCTOU:
            // a symlink at an allowed path could point to a disallowed target.
            if path.exists() {
                return invalid(format!("Path canonicalization failed: {e}"));
            }
            // Path does not exist yet — do manual resolution without symlink traversal.
            // This is safe because non-existent paths cannot be symlinks.
            let mut resolved = if path.is_absolute() {
                PathBuf::new()
            } else {
                std::env::current_dir().unwrap_or_default()
            };
            for component in path.components() {
                match component {
                    Component::ParentDir => {
                        return invalid("Directory traversal detected".into());
                    }
                    Component::Normal(c) => resolved.push(c),
                    Component::RootDir => resolved.push(Component::RootDir),
                    Component::CurDir => {}
                    Component::Prefix(p) => resolved.push(p.as_os_str()),
                }
            }
            resolved
        }
    };

    // Whitelist validation
    let allowed_paths = get_default_allowed_paths(&options.additional_allowed_paths);
    let within_allowed = allowed_paths.iter().any(|base| resolved.starts_with(base));

    if !within_allowed {
        return invalid(format!(
            "Path outside allowed directories: {}",
            resolved.display()
        ));
    }

    PathValidationResult {
        is_valid: true,
        resolved_path: resolved,
        reason: None,
    }
}

/// Validate a path and return the resolved path or error.
pub fn validate_path_or_err(
    raw_path: &str,
    options: &PathValidationOptions,
) -> Result<PathBuf, String> {
    let result = validate_path(raw_path, options);
    if result.is_valid {
        Ok(result.resolved_path)
    } else {
        Err(result
            .reason
            .unwrap_or_else(|| "Path validation failed".into()))
    }
}

fn get_default_allowed_paths(additional: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Config directory
    paths.push(crate::config::get_platform_config_dir());

    // Current working directory
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd);
    }

    // Temp directory
    paths.push(std::env::temp_dir());

    // Additional paths
    paths.extend(additional.iter().cloned());

    paths
}

// ---------------------------------------------------------------------------
// URL request path validation (for proxy/API paths)
// ---------------------------------------------------------------------------

/// Validate a URL request path for path traversal attacks.
///
/// Iteratively percent-decodes the path until stable, then rejects any path
/// containing `..` or null bytes. Iterative decoding catches double- and
/// triple-encoded traversal sequences such as `%252e%252e` (which decodes
/// one round to `%2e%2e` and a second round to `..`).
///
/// Iteration is capped at 8 rounds to prevent DoS on adversarial input.
/// Returns `true` if the path is safe.
pub fn is_safe_request_path(path: &str) -> bool {
    // Fast path: check raw path first
    if path.contains("..") || path.contains('\0') {
        return false;
    }

    // Iteratively decode to catch double/triple/N-layer percent-encoding.
    // Cap at 8 rounds — real paths have at most 1-2 encoding layers; this
    // bounds worst-case O(n * 8) work regardless of attacker input depth.
    let mut current = path.to_string();
    for _ in 0..8 {
        let decoded = simple_percent_decode(&current);
        if decoded.contains("..") || decoded.contains('\0') {
            return false;
        }
        if decoded == current {
            // Stable — no further encoding layers present.
            break;
        }
        current = decoded;
    }
    true
}

/// Minimal percent-decoder: converts `%XX` sequences to their byte values.
///
/// Only used for security checks — does not need to handle every edge case
/// of full URL parsing, just enough to catch encoded traversal sequences.
fn simple_percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- URL request path validation tests --

    #[test]
    fn safe_path_normal_routes() {
        assert!(is_safe_request_path("/v1/messages"));
        assert!(is_safe_request_path("/v1/models"));
        assert!(is_safe_request_path("/api/accounts"));
        assert!(is_safe_request_path("/health"));
        assert!(is_safe_request_path("/"));
    }

    #[test]
    fn rejects_raw_dot_dot() {
        assert!(!is_safe_request_path("/v1/../../admin"));
        assert!(!is_safe_request_path("/../etc/passwd"));
        assert!(!is_safe_request_path("/v1/messages/.."));
    }

    #[test]
    fn rejects_encoded_dot_dot() {
        // %2e = '.'
        assert!(!is_safe_request_path("/v1/%2e%2e/admin"));
        assert!(!is_safe_request_path("/v1/%2e./admin"));
        assert!(!is_safe_request_path("/v1/.%2e/admin"));
    }

    #[test]
    fn rejects_uppercase_encoded_dot_dot() {
        assert!(!is_safe_request_path("/v1/%2E%2E/admin"));
        assert!(!is_safe_request_path("/v1/%2E./admin"));
        assert!(!is_safe_request_path("/v1/.%2E/admin"));
    }

    #[test]
    fn rejects_mixed_case_encoded_dot_dot() {
        assert!(!is_safe_request_path("/v1/%2e%2E/admin"));
        assert!(!is_safe_request_path("/v1/%2E%2e/admin"));
    }

    #[test]
    fn rejects_null_byte_in_url() {
        assert!(!is_safe_request_path("/v1/messages%00"));
        assert!(!is_safe_request_path("/v1/messages\0"));
    }

    #[test]
    fn allows_single_dot_in_path() {
        assert!(is_safe_request_path("/v1/./messages"));
        assert!(is_safe_request_path("/v1/file.json"));
    }

    #[test]
    fn rejects_double_encoded_dot_dot() {
        // %252e%252e → (decode once) → %2e%2e → (decode again) → ..
        assert!(!is_safe_request_path("/v1/%252e%252e/admin"));
        assert!(!is_safe_request_path("/%252e%252e/etc/passwd"));
        assert!(!is_safe_request_path("/v1/%252E%252E/admin")); // uppercase
    }

    #[test]
    fn rejects_triple_encoded_dot_dot() {
        // %25252e → %252e → %2e → '.'
        assert!(!is_safe_request_path("/v1/%25252e%25252e/admin"));
    }

    #[test]
    fn rejects_double_encoded_slash_traversal() {
        // %252F%252e%252e%252F → %2F%2e%2e%2F → /../
        assert!(!is_safe_request_path("/v1/%252F%252e%252e%252F"));
    }

    #[test]
    fn rejects_mixed_double_and_single_encoding() {
        // One dot literal, one double-encoded
        assert!(!is_safe_request_path("/v1/%252e./admin"));
        assert!(!is_safe_request_path("/v1/.%252e/admin"));
    }

    #[test]
    fn rejects_double_encoded_null_byte() {
        // %2500 → '\0' after second decode
        assert!(!is_safe_request_path("/v1/messages%2500"));
    }

    #[test]
    fn allows_double_encoded_percent_in_path() {
        // %2525 → %25 → % (just a literal percent sign, no traversal)
        assert!(is_safe_request_path("/v1/name%2525with%2525percent"));
    }

    // -- Filesystem path validation tests --

    #[test]
    fn rejects_null_byte() {
        let opts = PathValidationOptions::default();
        let result = validate_path("/tmp/test\0evil", &opts);
        assert!(!result.is_valid);
        assert!(result.reason.unwrap().contains("Null byte"));
    }

    #[test]
    fn rejects_traversal() {
        let opts = PathValidationOptions::default();
        let result = validate_path("/tmp/../etc/passwd", &opts);
        assert!(!result.is_valid);
        assert!(result.reason.unwrap().contains("traversal"));
    }

    #[test]
    fn rejects_empty_when_not_allowed() {
        let opts = PathValidationOptions {
            allow_empty: false,
            ..Default::default()
        };
        let result = validate_path("", &opts);
        assert!(!result.is_valid);
    }

    #[test]
    fn allows_temp_path() {
        let tmp = std::env::temp_dir().join("bccf-test-validator");
        let opts = PathValidationOptions::default();
        let result = validate_path(tmp.to_str().unwrap(), &opts);
        assert!(result.is_valid);
    }
}
