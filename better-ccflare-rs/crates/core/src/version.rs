use std::sync::OnceLock;

/// Claude CLI version used in user-agent headers.
pub const CLAUDE_CLI_VERSION: &str = "2.1.37";

static CACHED_VERSION: OnceLock<String> = OnceLock::new();

/// Get the application version from environment or Cargo.toml.
pub fn get_version() -> &'static str {
    CACHED_VERSION.get_or_init(|| {
        // 1. Build-time injected version
        if let Ok(v) = std::env::var("BETTER_CCFLARE_VERSION") {
            return v;
        }

        // 2. Cargo package version (set at compile time)
        let cargo_version = env!("CARGO_PKG_VERSION");
        if !cargo_version.is_empty() {
            return cargo_version.to_string();
        }

        // 3. Fallback
        CLAUDE_CLI_VERSION.to_string()
    })
}

/// Extract Claude CLI version from a user-agent header.
///
/// ```
/// use bccf_core::version::extract_claude_version;
/// assert_eq!(extract_claude_version("claude-cli/2.0.60 (external, cli)"), Some("2.0.60".to_string()));
/// assert_eq!(extract_claude_version("Mozilla/5.0"), None);
/// ```
pub fn extract_claude_version(user_agent: &str) -> Option<String> {
    // Match claude-cli/X.Y.Z pattern
    let prefix = "claude-cli/";
    let start = user_agent.find(prefix)?;
    let version_start = start + prefix.len();
    let rest = &user_agent[version_start..];

    // Find the end of the version string (semver allows digits, dots, hyphens, plus, alpha)
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-' && c != '+')
        .unwrap_or(rest.len());

    let version = &rest[..end];
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_from_user_agent() {
        assert_eq!(
            extract_claude_version("claude-cli/2.0.60 (external, cli)"),
            Some("2.0.60".to_string())
        );
    }

    #[test]
    fn extract_version_none() {
        assert_eq!(extract_claude_version("Mozilla/5.0"), None);
    }

    #[test]
    fn extract_version_with_prerelease() {
        assert_eq!(
            extract_claude_version("claude-cli/2.1.37-beta.1"),
            Some("2.1.37-beta.1".to_string())
        );
    }

    #[test]
    fn get_version_returns_string() {
        let v = get_version();
        assert!(!v.is_empty());
    }
}
