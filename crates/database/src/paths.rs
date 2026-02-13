//! Database path resolution.
//!
//! Priority: `BETTER_CCFLARE_DB_PATH` env > default platform config dir.
//! Legacy: also checks `~/.config/ccflare/ccflare.db` for migration.

use std::path::PathBuf;

/// Default database filename.
const DB_FILENAME: &str = "better-ccflare.db";

/// Legacy database filename.
const LEGACY_DB_FILENAME: &str = "ccflare.db";

/// Resolve the database path.
///
/// Checks `BETTER_CCFLARE_DB_PATH` first, then falls back to
/// `<platform_config_dir>/better-ccflare/better-ccflare.db`.
pub fn resolve_db_path() -> PathBuf {
    if let Ok(custom) = std::env::var("BETTER_CCFLARE_DB_PATH") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }

    // Fallback: also check the older env var name
    if let Ok(custom) = std::env::var("CCFLARE_DB_PATH") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }

    bccf_core::config::get_platform_config_dir().join(DB_FILENAME)
}

/// Resolve the legacy ccflare database path.
pub fn resolve_legacy_db_path() -> PathBuf {
    bccf_core::config::get_legacy_config_dir().join(LEGACY_DB_FILENAME)
}

/// Ensure the parent directory for the database file exists.
pub fn ensure_db_dir(db_path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_db_path_default() {
        // Clear env to test default
        std::env::remove_var("BETTER_CCFLARE_DB_PATH");
        std::env::remove_var("CCFLARE_DB_PATH");
        let path = resolve_db_path();
        assert!(path.to_string_lossy().contains("better-ccflare"));
        assert!(path.to_string_lossy().ends_with("better-ccflare.db"));
    }

    #[test]
    fn resolve_db_path_custom_env() {
        std::env::set_var("BETTER_CCFLARE_DB_PATH", "/tmp/test.db");
        let path = resolve_db_path();
        assert_eq!(path, PathBuf::from("/tmp/test.db"));
        std::env::remove_var("BETTER_CCFLARE_DB_PATH");
    }

    #[test]
    fn legacy_path_contains_ccflare() {
        let path = resolve_legacy_db_path();
        assert!(path.to_string_lossy().contains("ccflare"));
    }
}
