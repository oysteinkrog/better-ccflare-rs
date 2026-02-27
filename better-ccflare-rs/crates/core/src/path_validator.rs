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

#[cfg(test)]
mod tests {
    use super::*;

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
